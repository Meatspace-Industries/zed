//! A record of each drawn frame's elements, for tools that inspect and drive an app from
//! outside it, such as a devtools bridge.
//!
//! Recording is off until [`Window::set_frame_recording`](crate::Window::set_frame_recording)
//! turns it on. While it is off, each element costs one branch.
//!
//! The design follows wsp-labs/gpui-agent (Apache-2.0). This version also records the element
//! id path, the view, the hitbox, and the resolved style.

use crate::{
    Bounds, ContentMask, EntityId, FocusId, GlobalElementId, HitboxId, Pixels, SharedString, Style,
};
use std::{panic::Location, rc::Rc};

/// What an element responds to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InteractionFlags {
    /// Has a click listener.
    pub click: bool,
    /// Has a mouse down, up, move or pressure listener.
    pub mouse: bool,
    /// Has a key or action listener.
    pub keys: bool,
    /// Has a hover style or hover listener.
    pub hover: bool,
    /// Tracks a focus handle.
    pub focusable: bool,
    /// Its focus handle is focused.
    pub focused: bool,
    /// Registered a text input handler while it painted.
    pub input: bool,
    /// Scrolls or listens for scroll wheel events.
    pub scroll: bool,
    /// Can be dragged.
    pub drag: bool,
    /// Accepts drops.
    pub drop: bool,
    /// Shows a tooltip on hover.
    pub tooltip: bool,
    /// Its hitbox stops the mouse from reaching what is behind it.
    pub blocks: bool,
}

/// One element drawn in the last frame.
#[derive(Clone, Debug)]
pub struct RecordedElement {
    /// Index of the parent element in the same frame.
    pub parent: Option<usize>,
    /// The Rust type of the element.
    pub type_name: &'static str,
    /// The element's global id path, if it has an id.
    pub id: Option<GlobalElementId>,
    /// Where the element was constructed.
    pub source: Option<&'static Location<'static>>,
    /// The view that rendered the element.
    pub view: Option<EntityId>,
    /// The `Render` type of that view.
    pub view_type: Option<&'static str>,
    /// Layout bounds in window coordinates.
    pub bounds: Bounds<Pixels>,
    /// The part of `bounds` inside its content mask. Empty when clipped away.
    pub visible_bounds: Bounds<Pixels>,
    /// The accessibility role the element declares.
    pub role: Option<accesskit::Role>,
    /// The accessible name the element gives itself.
    pub label: Option<SharedString>,
    /// The accessible value the element reports.
    pub value: Option<SharedString>,
    /// The element reports itself disabled.
    pub disabled: bool,
    /// The element's checked state, when it has one.
    pub toggled: Option<accesskit::Toggled>,
    /// What the element responds to.
    pub flags: InteractionFlags,
    /// The primary entry of the key context the element sets.
    pub key_context: Option<SharedString>,
    /// The focus handle the element tracks.
    pub focus: Option<FocusId>,
    /// The hitbox the element inserted. [`crate::Window::hit_test`] returns these ids.
    pub hitbox: Option<HitboxId>,
    /// The style the element resolved for this frame, with hover, active, and focus styles
    /// applied. Only elements built on `Interactivity`, such as `div`, report one.
    pub style: Option<Rc<Style>>,
    /// The lines of text the element painted itself, in paint order.
    pub texts: Vec<RecordedText>,
}

/// One line of text painted in the last frame.
#[derive(Clone, Debug)]
pub struct RecordedText {
    /// The line's bounds in window coordinates, before alignment.
    pub bounds: Bounds<Pixels>,
    /// The part of `bounds` inside its content mask.
    pub visible_bounds: Bounds<Pixels>,
    /// The painted string.
    pub text: SharedString,
}

/// What an `Interactivity` reports for the element being prepainted.
pub(crate) struct RecordedInteractivity {
    pub flags: InteractionFlags,
    pub key_context: Option<SharedString>,
    pub focus: Option<FocusId>,
    pub hitbox: Option<HitboxId>,
    pub style: Style,
}

pub(crate) fn visible(bounds: Bounds<Pixels>, mask: ContentMask<Pixels>) -> Bounds<Pixels> {
    bounds.intersect(&mask.bounds)
}

/// Copies a cached view's elements from the last frame, with their parent indices moved to
/// where the copies land.
pub(crate) fn reparent(
    elements: &[RecordedElement],
    old_start: usize,
    new_start: usize,
    outer_parent: Option<usize>,
) -> impl Iterator<Item = RecordedElement> + '_ {
    elements.iter().map(move |element| {
        let mut element = element.clone();
        element.parent = match element.parent {
            Some(parent) if parent >= old_start => Some(parent - old_start + new_start),
            _ => outer_parent,
        };
        element
    })
}
