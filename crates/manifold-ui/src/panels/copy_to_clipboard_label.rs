use crate::color;
use crate::node::{TextAlign, UIStyle};
use crate::tree::UITree;
use std::time::Instant;

const FLASH_DURATION_SECS: f32 = 0.8;

struct CopiedFlash {
    label_id: u32,
    original_text: String,
    start: Instant,
    applied: bool,
}

#[derive(Default)]
pub struct CopyToClipboardLabelState {
    flash: Option<CopiedFlash>,
}

impl CopyToClipboardLabelState {
    pub fn clear(&mut self) {
        self.flash = None;
    }

    pub fn label_id(&self) -> Option<u32> {
        self.flash.as_ref().map(|flash| flash.label_id)
    }

    pub fn trigger(&mut self, label_id: u32) {
        self.flash = Some(CopiedFlash {
            label_id,
            original_text: String::new(),
            start: Instant::now(),
            applied: false,
        });
    }

    pub fn sync(&mut self, tree: &mut UITree, font_size: u16, original_text: &str) {
        let Some(flash) = self.flash.as_mut() else {
            return;
        };

        if !flash.applied {
            flash.original_text = original_text.to_owned();
            tree.set_text(flash.label_id, "Copied");
            tree.set_style(flash.label_id, copied_style(font_size));
            flash.applied = true;
            return;
        }

        if flash.start.elapsed().as_secs_f32() > FLASH_DURATION_SECS {
            tree.set_text(flash.label_id, &flash.original_text);
            tree.set_style(flash.label_id, default_style(font_size));
            self.flash = None;
        }
    }
}

fn copied_style(font_size: u16) -> UIStyle {
    UIStyle {
        text_color: color::ACCENT_BLUE_C32,
        font_size,
        text_align: TextAlign::Right,
        ..UIStyle::default()
    }
}

fn default_style(font_size: u16) -> UIStyle {
    UIStyle {
        text_color: color::SLIDER_TEXT_C32,
        font_size,
        text_align: TextAlign::Right,
        ..UIStyle::default()
    }
}
