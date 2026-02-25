use std::collections::HashSet;

use gpui::{
    div, prelude::FluentBuilder, px, App, AppContext as _, Context, Entity,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Window,
};
use gpui_component::{
    checkbox::Checkbox, h_flex, input::InputState, radio::Radio, switch::Switch, v_flex,
    ActiveTheme, Disableable as _, Sizable,
};
use serde_json::{Map, Value};

use crate::node::{
    build_tree, build_tree_from_properties, default_value_for_schema, resolve_schema, ConfigNode,
    NodeKind,
};
use crate::NodeFilter;

/// A GUI form for editing a configuration derived from a JSON Schema.
///
/// Create one with [`SchemaForm::new`], passing a `schemars::Schema` and
/// the current value. Render it as a gpui `Entity<SchemaForm>`.
pub struct SchemaForm {
    nodes: Vec<ConfigNode>,
    expanded: HashSet<String>,
    defs: Map<String, Value>,
    filter: Option<Box<dyn NodeFilter>>,
    inputs: Vec<(String, Entity<InputState>)>,
    dirty: bool,
}

impl SchemaForm {
    pub fn new(
        schema: &schemars::Schema,
        value: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let root = schema.as_value();
        let defs = root
            .get("$defs")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let nodes = build_tree(schema, value);
        let mut expanded = HashSet::new();
        collect_expanded_paths(&nodes, &mut String::new(), &mut expanded);

        let mut form = SchemaForm {
            nodes,
            expanded,
            defs,
            filter: None,
            inputs: Vec::new(),
            dirty: false,
        };
        form.rebuild_inputs(window, cx);
        form
    }

    pub fn set_filter(
        &mut self,
        filter: impl NodeFilter + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.filter = Some(Box::new(filter));
        self.rebuild_inputs(window, cx);
        cx.notify();
    }

    pub fn to_value(&self) -> Value {
        nodes_to_value(&self.nodes)
    }

    pub fn to_config<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.to_value())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    // --- internal ---

    fn sync_inputs_to_nodes(&mut self, cx: &App) {
        for (path, input_entity) in &self.inputs {
            let text = input_entity.read(cx).value().to_string();
            let parts: Vec<&str> = path.split('.').collect();
            if let Some(node) = find_node_mut(&mut self.nodes, &parts) {
                let effective_kind = match &node.kind {
                    NodeKind::Option {
                        is_some: true,
                        inner_kind: Some(ik),
                    } => ik.as_ref().clone(),
                    other => other.clone(),
                };
                let new_value = match effective_kind {
                    NodeKind::String => Some(Value::String(text.clone())),
                    NodeKind::Integer => text.parse::<i64>().ok().map(|n| Value::Number(n.into())),
                    NodeKind::Float => text
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map(Value::Number),
                    _ => None,
                };
                if let Some(val) = new_value {
                    if node.value != val {
                        node.value = val;
                    }
                }
            }
        }
    }

    fn rebuild_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut new_inputs: Vec<(String, Entity<InputState>)> = Vec::new();
        let visible = self.visible_flat_nodes();

        for (path, node) in &visible {
            let effective_kind = match &node.kind {
                NodeKind::Option {
                    is_some: true,
                    inner_kind: Some(ik),
                } => Some(ik.as_ref()),
                k @ (NodeKind::String | NodeKind::Integer | NodeKind::Float) => Some(k),
                _ => None,
            };
            if let Some(ek) = effective_kind {
                let text = value_to_string(&node.value, ek);
                let entity =
                    if let Some(pos) = self.inputs.iter().position(|(p, _)| p == path) {
                        let e = self.inputs[pos].1.clone();
                        e.update(cx, |state, cx| {
                            if state.value().as_ref() != text {
                                state.set_value(text.clone(), window, cx);
                            }
                        });
                        e
                    } else {
                        let text_clone = text.clone();
                        let input = cx.new(|cx| {
                            let mut s = InputState::new(window, cx);
                            s.set_value(text_clone, window, cx);
                            s
                        });
                        cx.subscribe(&input, {
                            move |this: &mut SchemaForm, _entity, event: &gpui_component::input::InputEvent, cx| {
                                if matches!(event, gpui_component::input::InputEvent::Change) {
                                    this.dirty = true;
                                    this.sync_inputs_to_nodes(cx);
                                    cx.notify();
                                }
                            }
                        })
                        .detach();
                        input
                    };
                new_inputs.push((path.clone(), entity));
            }
        }
        self.inputs = new_inputs;
    }

    fn visible_flat_nodes(&self) -> Vec<(String, ConfigNode)> {
        let mut result = Vec::new();
        collect_visible_flat(
            &self.nodes,
            &mut String::new(),
            &self.expanded,
            self.filter.as_deref(),
            &mut result,
        );
        result
    }

    fn find_input(&self, path: &str) -> Option<&Entity<InputState>> {
        self.inputs.iter().find(|(p, _)| p == path).map(|(_, e)| e)
    }

    fn is_enabled(&self, path: &str) -> bool {
        match &self.filter {
            Some(f) => f.enabled(path),
            None => true,
        }
    }

    fn toggle_bool(&mut self, path: &str, cx: &mut Context<Self>) {
        let parts: Vec<&str> = path.split('.').collect();
        if let Some(node) = find_node_mut(&mut self.nodes, &parts) {
            if matches!(node.kind, NodeKind::Bool) {
                let current = node.value.as_bool().unwrap_or(false);
                node.value = Value::Bool(!current);
                self.dirty = true;
                cx.notify();
            }
        }
    }

    fn toggle_checkbox(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let parts: Vec<&str> = path.split('.').collect();
        if let Some(node) = find_node_mut(&mut self.nodes, &parts) {
            if let NodeKind::CheckboxItem { checked } = &mut node.kind {
                *checked = !*checked;
                node.value = Value::Bool(*checked);
            }
        }
        if let Some(dot) = path.rfind('.') {
            let parent_path = &path[..dot];
            let parts: Vec<&str> = parent_path.split('.').collect();
            rebuild_checkboxes_value(&mut self.nodes, &parts);
        }
        self.dirty = true;
        self.rebuild_inputs(window, cx);
        cx.notify();
    }

    fn select_radio(
        &mut self,
        parent_path: &str,
        variant_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parts: Vec<&str> = parent_path.split('.').collect();
        select_radio_in_nodes(&mut self.nodes, &parts, variant_name, &self.defs.clone());
        let selected_path = format!("{}.{}", parent_path, variant_name);
        self.expanded.insert(selected_path);
        self.dirty = true;
        self.rebuild_inputs(window, cx);
        cx.notify();
    }

    fn toggle_option(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let parts: Vec<&str> = path.split('.').collect();
        let (is_some, is_scalar, inner_schema) = {
            let node = match find_node(&self.nodes, &parts) {
                Some(n) => n,
                None => return,
            };
            match &node.kind {
                NodeKind::Option {
                    is_some,
                    inner_kind,
                } => (*is_some, inner_kind.is_some(), node.inner_schema.clone()),
                _ => return,
            }
        };

        if is_some {
            set_option_state_in_nodes(&mut self.nodes, &parts, false, Value::Null, Vec::new());
            self.expanded.remove(path);
        } else if let Some(ref schema) = inner_schema {
            let default = default_value_for_schema(schema, &self.defs);
            if is_scalar {
                set_option_state_in_nodes(&mut self.nodes, &parts, true, default, Vec::new());
            } else {
                let resolved = resolve_schema(schema, &self.defs);
                let children =
                    build_inner_children_from_schema(resolved, &default, &self.defs, parts.len());
                set_option_state_in_nodes(&mut self.nodes, &parts, true, default, children);
                self.expanded.insert(path.to_string());
            }
        }
        self.dirty = true;
        self.rebuild_inputs(window, cx);
        cx.notify();
    }

    fn toggle_expand(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.expanded.contains(path) {
            self.expanded.remove(path);
        } else {
            self.expanded.insert(path.to_string());
        }
        cx.notify();
    }

    fn render_nodes(
        &self,
        nodes: &[ConfigNode],
        prefix: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut container = v_flex().gap_1();

        for node in nodes {
            let path = if prefix.is_empty() {
                node.key.clone()
            } else {
                format!("{}.{}", prefix, node.key)
            };

            if let Some(ref f) = self.filter {
                if !f.visible(&path) {
                    continue;
                }
            }

            let enabled = self.is_enabled(&path);
            let row = self.render_node(node, &path, enabled, window, cx);
            container = container.child(row);
        }
        container
    }

    fn render_node(
        &self,
        node: &ConfigNode,
        path: &str,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let depth = node.depth;
        let indent = px(depth as f32 * 20.0);
        let is_expanded = self.expanded.contains(path);
        let opacity = if enabled { 1.0 } else { 0.5 };

        match &node.kind {
            NodeKind::Bool => {
                let checked = node.value.as_bool().unwrap_or(false);
                let path_owned = path.to_string();
                h_flex()
                    .pl(indent)
                    .py_0p5()
                    .gap_2()
                    .opacity(opacity)
                    .child(
                        Switch::new(SharedString::from(path.to_string()))
                            .checked(checked)
                            .disabled(!enabled)
                            .on_click(cx.listener({
                                let path = path_owned.clone();
                                move |this, _checked: &bool, _window, cx| {
                                    this.toggle_bool(&path, cx);
                                }
                            }))
                            .small(),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(format_label(&node.key)),
                    )
                    .when(node.description.is_some(), |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(node.description.clone().unwrap_or_default()),
                        )
                    })
                    .into_any_element()
            }
            NodeKind::String | NodeKind::Integer | NodeKind::Float => {
                self.render_scalar_field(node, path, indent, enabled, opacity, window, cx)
            }
            NodeKind::Struct { type_name } => {
                let path_owned = path.to_string();
                let arrow = if is_expanded { "▼" } else { "▶" };
                let mut col = v_flex().gap_1();
                col = col.child(
                    h_flex()
                        .id(SharedString::from(format!("hdr-{}", path)))
                        .pl(indent)
                        .py_0p5()
                        .gap_2()
                        .opacity(opacity)
                        .cursor_pointer()
                        .on_click(cx.listener({
                            let path = path_owned.clone();
                            move |this, _ev: &gpui::ClickEvent, _window, cx| {
                                this.toggle_expand(&path, cx);
                            }
                        }))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().foreground)
                                .child(format!(
                                    "{} {} ({})",
                                    arrow,
                                    format_label(&node.key),
                                    type_name
                                )),
                        )
                        .when(node.description.is_some(), |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(node.description.clone().unwrap_or_default()),
                            )
                        }),
                );
                if is_expanded {
                    col = col.child(self.render_nodes(&node.children, &path_owned, window, cx));
                }
                col.into_any_element()
            }
            NodeKind::Option {
                is_some,
                inner_kind,
            } => {
                let path_owned = path.to_string();

                if inner_kind.is_some() {
                    // Scalar option: switch + inline value
                    // Use a fixed row height (h_6 = 24px matches Input::small)
                    // so toggling between Some/None doesn't change the row height.
                    let mut row = h_flex()
                        .pl(indent)
                        .h_6()
                        .gap_2()
                        .items_center()
                        .opacity(opacity);

                    row = row.child(
                        Switch::new(SharedString::from(format!("{}-opt", path)))
                            .checked(*is_some)
                            .disabled(!enabled)
                            .on_click(cx.listener({
                                let path = path_owned.clone();
                                move |this, _checked: &bool, window, cx| {
                                    this.toggle_option(&path, window, cx);
                                }
                            }))
                            .small(),
                    );
                    row = row.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(format_label(&node.key)),
                    );

                    if *is_some {
                        if let Some(input_entity) = self.find_input(&path_owned) {
                            row = row.child(
                                gpui_component::input::Input::new(input_entity)
                                    .appearance(false)
                                    .xsmall()
                                    .text_color(cx.theme().muted_foreground)
                                    .disabled(!enabled),
                            );
                        }
                    } else {
                        row = row.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("None"),
                        );
                    }

                    if let Some(desc) = &node.description {
                        row = row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(desc.clone()),
                        );
                    }

                    row.into_any_element()
                } else {
                    // Struct option: expandable section with switch
                    let arrow = if *is_some && is_expanded {
                        "▼"
                    } else if *is_some {
                        "▶"
                    } else {
                        " "
                    };
                    let mut col = v_flex().gap_1();
                    col = col.child(
                        h_flex()
                            .pl(indent)
                            .py_0p5()
                            .gap_2()
                            .opacity(opacity)
                            .child(
                                Switch::new(SharedString::from(format!("{}-opt", path)))
                                    .checked(*is_some)
                                    .disabled(!enabled)
                                    .on_click(cx.listener({
                                        let path = path_owned.clone();
                                        move |this, _checked: &bool, window, cx| {
                                            this.toggle_option(&path, window, cx);
                                        }
                                    }))
                                    .small(),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("opt-lbl-{}", path)))
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .cursor_pointer()
                                    .when(*is_some, |el| {
                                        el.on_click(cx.listener({
                                            let path = path_owned.clone();
                                            move |this, _ev: &gpui::ClickEvent, _window, cx| {
                                                this.toggle_expand(&path, cx);
                                            }
                                        }))
                                    })
                                    .child(format!("{} {}", arrow, format_label(&node.key))),
                            )
                            .when(node.description.is_some(), |el| {
                                el.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(node.description.clone().unwrap_or_default()),
                                )
                            }),
                    );

                    if *is_some && is_expanded {
                        col = col
                            .child(self.render_nodes(&node.children, &path_owned, window, cx));
                    }
                    col.into_any_element()
                }
            }
            NodeKind::RadioGroup { .. } => {
                let path_owned = path.to_string();
                let arrow = if is_expanded { "▼" } else { "▶" };
                let mut col = v_flex().gap_1();
                col = col.child(
                    h_flex()
                        .id(SharedString::from(format!("rg-{}", path)))
                        .pl(indent)
                        .py_0p5()
                        .gap_2()
                        .opacity(opacity)
                        .cursor_pointer()
                        .on_click(cx.listener({
                            let path = path_owned.clone();
                            move |this, _ev: &gpui::ClickEvent, _window, cx| {
                                this.toggle_expand(&path, cx);
                            }
                        }))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().foreground)
                                .child(format!("{} {}", arrow, format_label(&node.key))),
                        )
                        .when(node.description.is_some(), |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(node.description.clone().unwrap_or_default()),
                            )
                        }),
                );
                if is_expanded {
                    for child in &node.children {
                        let child_path = format!("{}.{}", path_owned, child.key);
                        let child_enabled = self.is_enabled(&child_path);
                        col = col.child(self.render_radio_item(
                            child,
                            &child_path,
                            &path_owned,
                            child_enabled,
                            window,
                            cx,
                        ));
                    }
                }
                col.into_any_element()
            }
            NodeKind::RadioItem { .. } => div().into_any_element(),
            NodeKind::Checkboxes { .. } => {
                let path_owned = path.to_string();
                let arrow = if is_expanded { "▼" } else { "▶" };
                let mut col = v_flex().gap_1();
                col = col.child(
                    h_flex()
                        .id(SharedString::from(format!("cb-{}", path)))
                        .pl(indent)
                        .py_0p5()
                        .gap_2()
                        .opacity(opacity)
                        .cursor_pointer()
                        .on_click(cx.listener({
                            let path = path_owned.clone();
                            move |this, _ev: &gpui::ClickEvent, _window, cx| {
                                this.toggle_expand(&path, cx);
                            }
                        }))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().foreground)
                                .child(format!("{} {}", arrow, format_label(&node.key))),
                        )
                        .when(node.description.is_some(), |el| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(node.description.clone().unwrap_or_default()),
                            )
                        }),
                );
                if is_expanded {
                    for child in &node.children {
                        let child_path = format!("{}.{}", path_owned, child.key);
                        let child_enabled = self.is_enabled(&child_path);
                        col = col.child(self.render_checkbox_item(
                            child, &child_path, child_enabled, cx,
                        ));
                    }
                }
                col.into_any_element()
            }
            NodeKind::CheckboxItem { .. } => div().into_any_element(),
        }
    }

    fn render_scalar_field(
        &self,
        node: &ConfigNode,
        path: &str,
        indent: gpui::Pixels,
        enabled: bool,
        opacity: f32,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut row = h_flex()
            .pl(indent)
            .py_0p5()
            .gap_2()
            .items_center()
            .opacity(opacity);

        row = row.child(
            div()
                .text_sm()
                .text_color(cx.theme().foreground)
                .min_w(px(120.0))
                .child(format_label(&node.key)),
        );

        if let Some(input_entity) = self.find_input(path) {
            row = row.child(
                div().w(px(200.0)).child(
                    gpui_component::input::Input::new(input_entity)
                        .small()
                        .disabled(!enabled),
                ),
            );
        }

        if let Some(desc) = &node.description {
            row = row.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(desc.clone()),
            );
        }

        row.into_any_element()
    }

    fn render_radio_item(
        &self,
        node: &ConfigNode,
        path: &str,
        parent_path: &str,
        enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let NodeKind::RadioItem {
            selected,
            is_struct,
        } = &node.kind
        else {
            return div().into_any_element();
        };
        let depth = node.depth;
        let indent = px(depth as f32 * 20.0);
        let opacity = if enabled { 1.0 } else { 0.5 };
        let parent_owned = parent_path.to_string();
        let variant = node.key.clone();
        let is_expanded = self.expanded.contains(path);

        let mut col = v_flex().gap_1();

        let row = h_flex()
            .pl(indent)
            .py_0p5()
            .gap_2()
            .opacity(opacity)
            .child(
                Radio::new(SharedString::from(path.to_string()))
                    .label(format_label(&node.key))
                    .checked(*selected)
                    .disabled(!enabled)
                    .on_click(cx.listener({
                        let parent = parent_owned.clone();
                        let variant = variant.clone();
                        move |this, _checked: &bool, window, cx| {
                            this.select_radio(&parent, &variant, window, cx);
                        }
                    }))
                    .small(),
            );

        col = col.child(row);

        if *selected && *is_struct && is_expanded && !node.children.is_empty() {
            col = col.child(self.render_nodes(&node.children, path, window, cx));
        }

        col.into_any_element()
    }

    fn render_checkbox_item(
        &self,
        node: &ConfigNode,
        path: &str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let NodeKind::CheckboxItem { checked } = &node.kind else {
            return div().into_any_element();
        };
        let depth = node.depth;
        let indent = px(depth as f32 * 20.0);
        let opacity = if enabled { 1.0 } else { 0.5 };
        let path_owned = path.to_string();

        h_flex()
            .pl(indent)
            .py_0p5()
            .gap_2()
            .opacity(opacity)
            .child(
                Checkbox::new(SharedString::from(path.to_string()))
                    .label(format_label(&node.key))
                    .checked(*checked)
                    .disabled(!enabled)
                    .on_click(cx.listener({
                        let path = path_owned;
                        move |this, _checked: &bool, window, cx| {
                            this.toggle_checkbox(&path, window, cx);
                        }
                    }))
                    .small(),
            )
            .into_any_element()
    }
}

impl Render for SchemaForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_inputs_to_nodes(cx);

        let nodes = self.nodes.clone();
        v_flex()
            .id("schema-form-root")
            .p_4()
            .gap_2()
            .size_full()
            .overflow_y_scroll()
            .child(self.render_nodes(&nodes, "", window, cx))
    }
}

// --- Helper functions ---

fn format_label(key: &str) -> String {
    let mut result = String::new();
    let mut prev_was_lower = false;
    for (i, ch) in key.chars().enumerate() {
        if ch == '_' {
            result.push(' ');
            prev_was_lower = false;
        } else if ch.is_uppercase() && prev_was_lower {
            result.push(' ');
            result.push(ch);
            prev_was_lower = false;
        } else if i == 0 {
            result.push(ch.to_uppercase().next().unwrap());
            prev_was_lower = ch.is_lowercase();
        } else {
            result.push(ch);
            prev_was_lower = ch.is_lowercase();
        }
    }
    result
}

fn value_to_string(value: &Value, kind: &NodeKind) -> String {
    match kind {
        NodeKind::String => value.as_str().unwrap_or("").to_string(),
        NodeKind::Integer => match value {
            Value::Number(n) => n.to_string(),
            _ => "0".to_string(),
        },
        NodeKind::Float => match value {
            Value::Number(n) => n.to_string(),
            _ => "0.0".to_string(),
        },
        _ => value.to_string(),
    }
}

fn collect_expanded_paths(
    nodes: &[ConfigNode],
    prefix: &mut String,
    expanded: &mut HashSet<String>,
) {
    for node in nodes {
        let path = if prefix.is_empty() {
            node.key.clone()
        } else {
            format!("{}.{}", prefix, node.key)
        };

        match &node.kind {
            NodeKind::Struct { .. } | NodeKind::RadioGroup { .. } | NodeKind::Checkboxes { .. } => {
                expanded.insert(path.clone());
                collect_expanded_paths(&node.children, &mut path.clone(), expanded);
            }
            NodeKind::Option { is_some: true, .. } => {
                expanded.insert(path.clone());
                collect_expanded_paths(&node.children, &mut path.clone(), expanded);
            }
            NodeKind::RadioItem {
                selected: true,
                is_struct: true,
            } if !node.children.is_empty() => {
                expanded.insert(path.clone());
                collect_expanded_paths(&node.children, &mut path.clone(), expanded);
            }
            _ => {}
        }
    }
}

fn collect_visible_flat(
    nodes: &[ConfigNode],
    prefix: &mut String,
    expanded: &HashSet<String>,
    filter: Option<&dyn NodeFilter>,
    result: &mut Vec<(String, ConfigNode)>,
) {
    for node in nodes {
        let path = if prefix.is_empty() {
            node.key.clone()
        } else {
            format!("{}.{}", prefix, node.key)
        };

        if let Some(f) = filter {
            if !f.visible(&path) {
                continue;
            }
        }

        result.push((path.clone(), node.clone()));

        let is_expanded = expanded.contains(&path);
        if is_expanded && !node.children.is_empty() {
            collect_visible_flat(
                &node.children,
                &mut path.clone(),
                expanded,
                filter,
                result,
            );
        }
    }
}

fn find_node<'a>(nodes: &'a [ConfigNode], path_parts: &[&str]) -> Option<&'a ConfigNode> {
    let (&first, rest) = path_parts.split_first()?;
    for node in nodes {
        if node.key == first {
            if rest.is_empty() {
                return Some(node);
            } else {
                return find_node(&node.children, rest);
            }
        }
    }
    None
}

fn find_node_mut<'a>(
    nodes: &'a mut [ConfigNode],
    path_parts: &[&str],
) -> Option<&'a mut ConfigNode> {
    let (&first, rest) = path_parts.split_first()?;
    for node in nodes.iter_mut() {
        if node.key == first {
            if rest.is_empty() {
                return Some(node);
            } else {
                return find_node_mut(&mut node.children, rest);
            }
        }
    }
    None
}

fn set_option_state_in_nodes(
    nodes: &mut [ConfigNode],
    path_parts: &[&str],
    is_some: bool,
    value: Value,
    children: Vec<ConfigNode>,
) {
    let Some((&first, rest)) = path_parts.split_first() else {
        return;
    };
    for node in nodes.iter_mut() {
        if node.key == first {
            if rest.is_empty() {
                if let NodeKind::Option {
                    is_some: ref mut s, ..
                } = node.kind
                {
                    *s = is_some;
                }
                node.value = value;
                node.children = children;
                return;
            } else {
                set_option_state_in_nodes(&mut node.children, rest, is_some, value, children);
                return;
            }
        }
    }
}

fn select_radio_in_nodes(
    nodes: &mut [ConfigNode],
    path_parts: &[&str],
    variant_name: &str,
    defs: &Map<String, Value>,
) {
    let Some((&first, rest)) = path_parts.split_first() else {
        return;
    };
    for node in nodes.iter_mut() {
        if node.key == first {
            if rest.is_empty() {
                let child_depth = node.depth + 1;
                for child in &mut node.children {
                    if let NodeKind::RadioItem {
                        selected: ref mut s,
                        is_struct,
                    } = child.kind
                    {
                        let is_match = child.key == variant_name;
                        *s = is_match;
                        child.value = Value::Bool(is_match);

                        if is_struct {
                            if is_match {
                                if let Some(ref schema) = child.inner_schema {
                                    let default_val = default_value_for_schema(schema, defs);
                                    if let Some(props) =
                                        schema.get("properties").and_then(|v| v.as_object())
                                    {
                                        child.children = build_tree_from_properties(
                                            props,
                                            schema,
                                            defs,
                                            default_val.as_object(),
                                            child_depth + 1,
                                        );
                                    }
                                }
                            } else {
                                child.children.clear();
                            }
                        }
                    }
                }

                let selected_child = node.children.iter().find(|c| c.key == variant_name);
                if let Some(child) = selected_child {
                    if let NodeKind::RadioItem {
                        is_struct: true, ..
                    } = &child.kind
                    {
                        if let Some(ref schema) = child.inner_schema {
                            let default_val = default_value_for_schema(schema, defs);
                            let mut obj = Map::new();
                            obj.insert(variant_name.to_string(), default_val);
                            node.value = Value::Object(obj);
                        }
                    } else {
                        node.value = Value::String(variant_name.to_string());
                    }
                }
                return;
            } else {
                select_radio_in_nodes(&mut node.children, rest, variant_name, defs);
                return;
            }
        }
    }
}

fn rebuild_checkboxes_value(nodes: &mut [ConfigNode], path_parts: &[&str]) {
    let Some((&first, rest)) = path_parts.split_first() else {
        return;
    };
    for node in nodes.iter_mut() {
        if node.key == first {
            if rest.is_empty() {
                if matches!(node.kind, NodeKind::Checkboxes { .. }) {
                    node.value = Value::Array(
                        node.children
                            .iter()
                            .filter(|c| matches!(c.kind, NodeKind::CheckboxItem { checked: true }))
                            .map(|c| Value::String(c.key.clone()))
                            .collect(),
                    );
                }
                return;
            } else {
                rebuild_checkboxes_value(&mut node.children, rest);
                return;
            }
        }
    }
}

fn nodes_to_value(nodes: &[ConfigNode]) -> Value {
    let mut map = Map::new();
    for node in nodes {
        let val = match &node.kind {
            NodeKind::Struct { .. } => nodes_to_value(&node.children),
            NodeKind::Option { is_some, .. } => {
                if *is_some {
                    if node.children.len() == 1 && node.children[0].key == "value" {
                        node_to_value(&node.children[0])
                    } else if !node.children.is_empty() {
                        nodes_to_value(&node.children)
                    } else {
                        node.value.clone()
                    }
                } else {
                    Value::Null
                }
            }
            NodeKind::RadioGroup { .. } => radio_group_to_value(node),
            NodeKind::Checkboxes { .. } => Value::Array(
                node.children
                    .iter()
                    .filter(|c| matches!(c.kind, NodeKind::CheckboxItem { checked: true }))
                    .map(|c| Value::String(c.key.clone()))
                    .collect(),
            ),
            _ => node_to_value(node),
        };
        map.insert(node.key.clone(), val);
    }
    Value::Object(map)
}

fn radio_group_to_value(node: &ConfigNode) -> Value {
    let selected = node
        .children
        .iter()
        .find(|c| matches!(c.kind, NodeKind::RadioItem { selected: true, .. }));
    match selected {
        Some(child) => match &child.kind {
            NodeKind::RadioItem {
                is_struct: true, ..
            } if !child.children.is_empty() => {
                let mut obj = Map::new();
                obj.insert(child.key.clone(), nodes_to_value(&child.children));
                Value::Object(obj)
            }
            _ => Value::String(child.key.clone()),
        },
        None => node.value.clone(),
    }
}

fn node_to_value(node: &ConfigNode) -> Value {
    match &node.kind {
        NodeKind::RadioGroup { .. } => radio_group_to_value(node),
        NodeKind::Checkboxes { .. } => Value::Array(
            node.children
                .iter()
                .filter(|c| matches!(c.kind, NodeKind::CheckboxItem { checked: true }))
                .map(|c| Value::String(c.key.clone()))
                .collect(),
        ),
        _ => node.value.clone(),
    }
}

fn build_inner_children_from_schema(
    schema: &Value,
    value: &Value,
    defs: &Map<String, Value>,
    depth: usize,
) -> Vec<ConfigNode> {
    let type_str = schema
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if type_str == "object" {
        if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
            return build_tree_from_properties(props, schema, defs, value.as_object(), depth);
        }
    }
    vec![crate::node::build_node_pub("value", schema, value, defs, depth)]
}
