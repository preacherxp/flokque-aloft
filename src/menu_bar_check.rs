//! The green check on the menu bar glyph while Aloft keeps the Mac awake. The
//! glyph is a template image, which macOS colours for the menu bar but which
//! can't hold a colour of its own, so the check is an image view on the status
//! item's button, in the gap icons/menu-bar-on.png leaves for it.

use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSImage, NSImageView, NSStatusItem, NSWindowOrderingMode};
use objc2_foundation::{NSData, NSPoint, NSRect, NSSize};
use std::cell::RefCell;

/// The check's size in points, and how far it sits past the glyph's bottom right.
const SIZE: f64 = 9.0;
const OVERHANG: f64 = 0.5;

thread_local! {
    static CHECK: RefCell<Option<Retained<NSImageView>>> = const { RefCell::new(None) };
}

/// Shows the check at the glyph's bottom right, or takes it away. On the main thread.
pub fn show(item: &NSStatusItem, on: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    CHECK.with_borrow_mut(|check| {
        if let Some(view) = check.take() {
            view.removeFromSuperview();
        }
        let Some(button) = item.button(mtm).filter(|_| on) else {
            return;
        };
        let Some(cell) = button.cell() else { return };
        let data = NSData::with_bytes(include_bytes!("../icons/check.png"));
        let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
            return;
        };
        image.setSize(NSSize::new(SIZE, SIZE));
        let glyph = cell.imageRectForBounds(button.bounds());
        let x = glyph.origin.x + glyph.size.width - SIZE + OVERHANG;
        let y = if button.isFlipped() {
            glyph.origin.y + glyph.size.height - SIZE + OVERHANG
        } else {
            glyph.origin.y - OVERHANG
        };
        let view = NSImageView::imageViewWithImage(&image, mtm);
        view.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(SIZE, SIZE)));
        // Under tray-icon's own view, which takes the clicks.
        button.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);
        *check = Some(view);
    });
}
