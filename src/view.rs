use std::collections::HashSet;

use gpui::{
    actions, div, px, App, AppContext as _, Context, Entity, Focusable,
    FocusHandle, InteractiveElement as _, IntoElement, KeyBinding, ParentElement, Render,
    SharedString, StatefulInteractiveElement as _, Styled, Window,
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

/// Fixed min-width for the controls column so descriptions align vertically.
const CONTROLS_WIDTH: f32 = 400.0;

// --- Actions ---

actions!(
    schema_form,
    [
        SelectUp,
        SelectDown,
        ToggleOrExpand,
        ConfirmAction,
        ExpandNode,
        CollapseNode,
        CancelEdit,
        DeleteOption,
    ]
);

/// Register key bindings for SchemaForm keyboard navigation.
/// Call this once in your application's setup, after `gpui_component::init(cx)`.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectUp, Some("SchemaForm")),
        KeyBinding::new("down", SelectDown, Some("SchemaForm")),
        KeyBinding::new("space", ToggleOrExpand, Some("SchemaForm")),
        KeyBinding::new("enter", ConfirmAction, Some("SchemaForm")),
        KeyBinding::new("right", ExpandNode, Some("SchemaForm")),
        KeyBinding::new("left", CollapseNode, Some("SchemaForm")),
        KeyBinding::new("escape", CancelEdit, Some("SchemaForm")),
        KeyBinding::new("backspace", DeleteOption, Some("SchemaForm")),
        KeyBinding::new("delete", DeleteOption, Some("SchemaForm")),
    ]);
}

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
    focus_handle: FocusHandle,
    selected: usize,
    editing_path: Option<String>,
}

impl Focusable for SchemaForm {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
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
            focus_handle: cx.focus_handle(),
            selected: 0,
            editing_path: None,
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

    // --- Input sync ---

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
                                match event {
                                    gpui_component::input::InputEvent::Change => {
                                        this.dirty = true;
                                        this.sync_inputs_to_nodes(cx);
                                        cx.notify();
                                    }
                                    gpui_component::input::InputEvent::Focus => {
                                        // editing_path is set by focus_input_at;
                                        // mouse-driven focus on an Input is prevented
                                        // because we only render the Input widget when
                                        // editing_path matches. So this is a no-op guard.
                                        cx.notify();
                                    }
                                    gpui_component::input::InputEvent::Blur => {
                                        this.editing_path = None;
                                        cx.notify();
                                    }
                                    _ => {}
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

    // --- Focus helpers ---

    fn editing(&self) -> bool {
        self.editing_path.is_some()
    }

    fn focus_input_at(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_path = Some(path.to_string());
        // Focus the input entity — it exists even when rendered as text
        if let Some(input_entity) = self.find_input(path).cloned() {
            input_entity.update(cx, |state, cx| {
                state.focus(window, cx);
            });
        }
        cx.notify();
    }

    fn blur_active_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing_path = None;
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_flat_nodes().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    // --- Mutations ---

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

    // --- Action handlers ---

    fn on_select_up(&mut self, _: &SelectUp, _window: &mut Window, cx: &mut Context<Self>) {
        if self.editing() {
            cx.propagate();
            return;
        }
        if self.selected > 0 {
            self.selected -= 1;
            cx.notify();
        }
    }

    fn on_select_down(&mut self, _: &SelectDown, _window: &mut Window, cx: &mut Context<Self>) {
        if self.editing() {
            cx.propagate();
            return;
        }
        let len = self.visible_flat_nodes().len();
        if len > 0 && self.selected < len - 1 {
            self.selected += 1;
            cx.notify();
        }
    }

    fn on_toggle_or_expand(
        &mut self,
        _: &ToggleOrExpand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing() {
            cx.propagate();
            return;
        }
        let visible = self.visible_flat_nodes();
        let Some((path, node)) = visible.get(self.selected).cloned() else {
            return;
        };
        if !self.is_enabled(&path) {
            return;
        }
        match &node.kind {
            NodeKind::Bool => self.toggle_bool(&path, cx),
            NodeKind::CheckboxItem { .. } => self.toggle_checkbox(&path, window, cx),
            NodeKind::RadioItem { .. } => {
                if let Some(dot) = path.rfind('.') {
                    let parent = path[..dot].to_string();
                    let variant = node.key.clone();
                    self.select_radio(&parent, &variant, window, cx);
                }
            }
            NodeKind::Option { .. } => self.toggle_option(&path, window, cx),
            NodeKind::Struct { .. } | NodeKind::RadioGroup { .. } | NodeKind::Checkboxes { .. } => {
                self.toggle_expand(&path, cx);
            }
            _ => {}
        }
        self.clamp_selection();
    }

    fn on_confirm(
        &mut self,
        _: &ConfirmAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing() {
            self.blur_active_input(window, cx);
            return;
        }
        let visible = self.visible_flat_nodes();
        let Some((path, node)) = visible.get(self.selected).cloned() else {
            return;
        };
        if !self.is_enabled(&path) {
            return;
        }
        match &node.kind {
            NodeKind::String | NodeKind::Integer | NodeKind::Float => {
                self.focus_input_at(&path, window, cx);
            }
            NodeKind::Option {
                is_some: true,
                inner_kind: Some(_),
            } => {
                self.focus_input_at(&path, window, cx);
            }
            NodeKind::Bool => self.toggle_bool(&path, cx),
            NodeKind::Struct { .. } | NodeKind::RadioGroup { .. } | NodeKind::Checkboxes { .. } => {
                self.toggle_expand(&path, cx);
            }
            NodeKind::Option { .. } => self.toggle_option(&path, window, cx),
            NodeKind::RadioItem { .. } => {
                if let Some(dot) = path.rfind('.') {
                    let parent = path[..dot].to_string();
                    let variant = node.key.clone();
                    self.select_radio(&parent, &variant, window, cx);
                }
            }
            NodeKind::CheckboxItem { .. } => self.toggle_checkbox(&path, window, cx),
        }
        self.clamp_selection();
    }

    fn on_expand_node(
        &mut self,
        _: &ExpandNode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing() {
            cx.propagate();
            return;
        }
        let visible = self.visible_flat_nodes();
        let Some((path, node)) = visible.get(self.selected).cloned() else {
            return;
        };
        let expandable = matches!(
            &node.kind,
            NodeKind::Struct { .. }
                | NodeKind::RadioGroup { .. }
                | NodeKind::Checkboxes { .. }
                | NodeKind::Option {
                    is_some: true,
                    inner_kind: None,
                }
        ) || matches!(
            &node.kind,
            NodeKind::RadioItem {
                selected: true,
                is_struct: true,
            }
        );
        if expandable && !self.expanded.contains(&path) {
            self.expanded.insert(path);
            cx.notify();
        }
    }

    fn on_collapse_node(
        &mut self,
        _: &CollapseNode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing() {
            cx.propagate();
            return;
        }
        let visible = self.visible_flat_nodes();
        let Some((path, node)) = visible.get(self.selected).cloned() else {
            return;
        };
        let expandable = matches!(
            &node.kind,
            NodeKind::Struct { .. }
                | NodeKind::RadioGroup { .. }
                | NodeKind::Checkboxes { .. }
                | NodeKind::Option {
                    is_some: true,
                    inner_kind: None,
                }
        ) || matches!(
            &node.kind,
            NodeKind::RadioItem {
                selected: true,
                is_struct: true,
            }
        );
        if expandable && self.expanded.contains(&path) {
            self.expanded.remove(&path);
            self.clamp_selection();
            cx.notify();
        } else if let Some(dot) = path.rfind('.') {
            // On a leaf: collapse parent and select it
            let parent = path[..dot].to_string();
            if self.expanded.contains(&parent) {
                self.expanded.remove(&parent);
                let visible_after = self.visible_flat_nodes();
                if let Some(idx) = visible_after.iter().position(|(p, _)| *p == parent) {
                    self.selected = idx;
                }
                self.clamp_selection();
                cx.notify();
            }
        }
    }

    fn on_cancel_edit(
        &mut self,
        _: &CancelEdit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing() {
            self.blur_active_input(window, cx);
        }
    }

    fn on_delete_option(
        &mut self,
        _: &DeleteOption,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.editing() {
            cx.propagate();
            return;
        }
        let visible = self.visible_flat_nodes();
        let Some((path, node)) = visible.get(self.selected).cloned() else {
            return;
        };
        if let NodeKind::Option { is_some: true, .. } = &node.kind {
            if self.is_enabled(&path) {
                self.toggle_option(&path, window, cx);
                self.clamp_selection();
            }
        }
    }

    // --- Rendering ---

    fn render_flat_row(
        &self,
        node: &ConfigNode,
        path: &str,
        is_selected: bool,
        enabled: bool,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let depth = node.depth;
        let indent = px(depth as f32 * 20.0);
        let opacity = if enabled { 1.0 } else { 0.5 };
        let is_expanded = self.expanded.contains(path);

        let controls = self.render_row_controls(node, path, enabled, is_expanded, cx);

        let idx = index;
        let mut row = h_flex()
            .id(SharedString::from(format!("row-{}", idx)))
            .h_6()
            .items_center()
            .opacity(opacity)
            .w_full()
            .rounded_sm()
            .cursor_pointer()
            .on_click(cx.listener({
                move |this, _ev: &gpui::ClickEvent, window, cx| {
                    this.selected = idx;
                    this.focus_handle.focus(window);
                    cx.notify();
                }
            }));

        if is_selected && !self.editing() {
            row = row.bg(cx.theme().list_active);
        }

        // Left column: fixed width, indent is internal padding
        row = row.child(
            h_flex()
                .w(px(CONTROLS_WIDTH))
                .flex_shrink_0()
                .pl(indent)
                .gap_2()
                .items_center()
                .child(controls),
        );

        // Right column: description, left-aligned
        if let Some(desc) = &node.description {
            row = row.child(
                div()
                    .pl_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(desc.clone()),
            );
        }

        row.into_any_element()
    }

    fn render_row_controls(
        &self,
        node: &ConfigNode,
        path: &str,
        enabled: bool,
        is_expanded: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let path_owned = path.to_string();

        match &node.kind {
            NodeKind::Bool => {
                let checked = node.value.as_bool().unwrap_or(false);
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Switch::new(SharedString::from(path.to_string()))
                            .checked(checked)
                            .disabled(!enabled)
                            .on_click(cx.listener({
                                let p = path_owned.clone();
                                move |this, _, _, cx| this.toggle_bool(&p, cx)
                            }))
                            .small(),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(format_label(&node.key)),
                    )
                    .into_any_element()
            }

            NodeKind::String | NodeKind::Integer | NodeKind::Float => {
                let mut row = h_flex().gap_2().items_center().child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().foreground)
                        .min_w(px(120.0))
                        .child(format_label(&node.key)),
                );
                let is_editing_this = self.editing_path.as_deref() == Some(path);
                if is_editing_this {
                    if let Some(input_entity) = self.find_input(path) {
                        row = row.child(
                            div().w(px(200.0)).child(
                                gpui_component::input::Input::new(input_entity)
                                    .appearance(false)
                                    .xsmall()
                                    .disabled(!enabled),
                            ),
                        );
                    }
                } else {
                    let text = value_to_string(&node.value, &node.kind);
                    row = row.child(
                        div()
                            .w(px(200.0))
                            .h_5()
                            .items_center()
                            .pl(px(4.0))
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(text),
                    );
                }
                row.into_any_element()
            }

            NodeKind::Struct { type_name } => {
                let arrow = if is_expanded { "▼" } else { "▶" };
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        div()
                            .id(SharedString::from(format!("{}-arrow", path)))
                            .text_sm()
                            .cursor_pointer()
                            .child(arrow)
                            .on_click(cx.listener({
                                let p = path_owned.clone();
                                move |this, _, _, cx| this.toggle_expand(&p, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(format!("{} ({})", format_label(&node.key), type_name)),
                    )
                    .into_any_element()
            }

            NodeKind::Option {
                is_some,
                inner_kind,
            } => {
                if inner_kind.is_some() {
                    // Scalar option
                    let mut row = h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Switch::new(SharedString::from(format!("{}-opt", path)))
                                .checked(*is_some)
                                .disabled(!enabled)
                                .on_click(cx.listener({
                                    let p = path_owned.clone();
                                    move |this, _, window, cx| {
                                        this.toggle_option(&p, window, cx);
                                    }
                                }))
                                .small(),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().foreground)
                                .min_w(px(120.0))
                                .child(format_label(&node.key)),
                        );

                    if *is_some {
                        let is_editing_this = self.editing_path.as_deref() == Some(path);
                        if is_editing_this {
                            if let Some(input_entity) = self.find_input(&path_owned) {
                                row = row.child(
                                    div().w(px(200.0)).child(
                                        gpui_component::input::Input::new(input_entity)
                                            .appearance(false)
                                            .xsmall()
                                            .disabled(!enabled),
                                    ),
                                );
                            }
                        } else {
                            let ik = inner_kind.as_ref().unwrap();
                            let text = value_to_string(&node.value, ik);
                            row = row.child(
                                div()
                                    .w(px(200.0))
                                    .h_5()
                                    .items_center()
                                    .pl(px(4.0))
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(text),
                            );
                        }
                    } else {
                        row = row.child(
                            div()
                                .w(px(200.0))
                                .h_5()
                                .items_center()
                                .pl(px(4.0))
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("None"),
                        );
                    }
                    row.into_any_element()
                } else {
                    // Struct option
                    let arrow = if *is_some && is_expanded {
                        "▼"
                    } else if *is_some {
                        "▶"
                    } else {
                        " "
                    };
                    let mut row = h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Switch::new(SharedString::from(format!("{}-opt", path)))
                                .checked(*is_some)
                                .disabled(!enabled)
                                .on_click(cx.listener({
                                    let p = path_owned.clone();
                                    move |this, _, window, cx| {
                                        this.toggle_option(&p, window, cx);
                                    }
                                }))
                                .small(),
                        );
                    if *is_some {
                        row = row.child(
                            div()
                                .id(SharedString::from(format!("{}-arrow", path)))
                                .text_sm()
                                .cursor_pointer()
                                .child(arrow)
                                .on_click(cx.listener({
                                    let p = path_owned.clone();
                                    move |this, _, _, cx| this.toggle_expand(&p, cx)
                                })),
                        );
                    }
                    row = row.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(format_label(&node.key)),
                    );
                    row.into_any_element()
                }
            }

            NodeKind::RadioGroup { .. } => {
                let arrow = if is_expanded { "▼" } else { "▶" };
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        div()
                            .id(SharedString::from(format!("{}-arrow", path)))
                            .text_sm()
                            .cursor_pointer()
                            .child(arrow)
                            .on_click(cx.listener({
                                let p = path_owned.clone();
                                move |this, _, _, cx| this.toggle_expand(&p, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(format_label(&node.key)),
                    )
                    .into_any_element()
            }

            NodeKind::RadioItem { selected, .. } => {
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Radio::new(SharedString::from(path.to_string()))
                            .label(format_label(&node.key))
                            .checked(*selected)
                            .disabled(!enabled)
                            .on_click(cx.listener({
                                let parent = path
                                    .rfind('.')
                                    .map(|i| path[..i].to_string())
                                    .unwrap_or_default();
                                let variant = node.key.clone();
                                move |this, _, window, cx| {
                                    this.select_radio(&parent, &variant, window, cx);
                                }
                            }))
                            .small(),
                    )
                    .into_any_element()
            }

            NodeKind::CheckboxItem { checked } => {
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Checkbox::new(SharedString::from(path.to_string()))
                            .label(format_label(&node.key))
                            .checked(*checked)
                            .disabled(!enabled)
                            .on_click(cx.listener({
                                let p = path_owned;
                                move |this, _, window, cx| {
                                    this.toggle_checkbox(&p, window, cx);
                                }
                            }))
                            .small(),
                    )
                    .into_any_element()
            }

            NodeKind::Checkboxes { .. } => {
                let arrow = if is_expanded { "▼" } else { "▶" };
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        div()
                            .id(SharedString::from(format!("{}-arrow", path)))
                            .text_sm()
                            .cursor_pointer()
                            .child(arrow)
                            .on_click(cx.listener({
                                let p = path_owned;
                                move |this, _, _, cx| this.toggle_expand(&p, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().foreground)
                            .child(format_label(&node.key)),
                    )
                    .into_any_element()
            }
        }
    }
}

impl Render for SchemaForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_inputs_to_nodes(cx);
        self.clamp_selection();

        let visible = self.visible_flat_nodes();
        let mut rows = v_flex().gap_0p5();

        for (idx, (path, node)) in visible.iter().enumerate() {
            let is_selected = idx == self.selected;
            let enabled = self.is_enabled(path);
            let row = self.render_flat_row(node, path, is_selected, enabled, idx, window, cx);
            rows = rows.child(row);
        }

        div()
            .id("schema-form-root")
            .track_focus(&self.focus_handle)
            .key_context("SchemaForm")
            .on_action(cx.listener(Self::on_select_up))
            .on_action(cx.listener(Self::on_select_down))
            .on_action(cx.listener(Self::on_toggle_or_expand))
            .on_action(cx.listener(Self::on_confirm))
            .on_action(cx.listener(Self::on_expand_node))
            .on_action(cx.listener(Self::on_collapse_node))
            .on_action(cx.listener(Self::on_cancel_edit))
            .on_action(cx.listener(Self::on_delete_option))
            .on_click(cx.listener(|this, _ev: &gpui::ClickEvent, window, _cx| {
                this.focus_handle.focus(window);
            }))
            .p_4()
            .size_full()
            .overflow_y_scroll()
            .child(rows)
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
