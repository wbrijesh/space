//! The gallery's Tokens page: colors, spacing, radii, type scale, motion.

use gpui::*;

use crate::components::loaders::{Dots, Spinner};
use crate::theme::{color, motion, radius, size, space, text};

fn section(title: &'static str) -> Div {
    div().flex().flex_col().gap_3().child(div().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).child(title))
}

fn caption(s: impl Into<SharedString>) -> Div {
    div().font_family(text::MONO_FONT).text_size(text::XS).text_color(color::text_3()).child(s.into())
}

pub fn page() -> Div {
    let mut swatches = div().flex().flex_wrap().gap_3();
    for (name, hex, doc) in color::ALL {
        swatches = swatches.child(
            div()
                .w(px(168.))
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div().h(px(44.)).rounded(radius::LG).border_1().border_color(color::border()).bg(Hsla::from(rgba(*hex))),
                )
                .child(div().text_size(text::SM).child(*name))
                .child(caption(format!("#{:08x}", hex)))
                .child(div().text_size(text::XS).text_color(color::text_2()).child(*doc)),
        );
    }

    let mut spacing = div().flex().flex_col().gap_2();
    for (name, v) in space::ALL {
        spacing = spacing.child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().w(px(48.)).child(caption(*name)))
                .child(div().w(px(*v * 4.)).h(px(10.)).rounded(radius::XS).bg(color::accent()))
                .child(caption(format!("{v}px"))),
        );
    }

    let mut radii = div().flex().flex_wrap().gap_4();
    for (name, r) in radius::ALL {
        radii = radii.child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .child(div().size(px(56.)).rounded(*r).bg(color::surface_2()).border_1().border_color(color::border_strong()))
                .child(caption(format!("{name} {}", f32::from(*r)))),
        );
    }

    let mut type_scale = div().flex().flex_col().gap_2();
    for (name, s) in text::ALL {
        type_scale = type_scale.child(
            div()
                .flex()
                .items_baseline()
                .gap_4()
                .child(div().w(px(90.)).child(caption(format!("{name} {}", f32::from(*s)))))
                .child(div().text_size(*s).child("What should we work on?")),
        );
    }
    type_scale = type_scale
        .child(caption(format!("UI font {}  ·  mono {}", text::UI_FONT, text::MONO_FONT)))
        .child(div().font_family(text::MONO_FONT).text_size(text::CODE).child("cargo test --workspace  # mono"));

    let mut motion_rows = div().flex().flex_col().gap_3();
    for (name, d, doc) in motion::ALL {
        motion_rows = motion_rows.child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(div().w(px(150.)).child(caption(*name)))
                .child(div().w(px(70.)).child(caption(format!("{} ms", d.as_millis()))))
                .child(div().text_size(text::SM).text_color(color::text_2()).child(*doc)),
        );
    }
    motion_rows = motion_rows.child(
        div()
            .flex()
            .items_center()
            .gap_6()
            .child(Spinner::new(size::SPINNER_SM))
            .child(Spinner::new(size::SPINNER_MD))
            .child(Spinner::new(size::SPINNER_LG))
            .child(Dots::new()),
    );

    div()
        .flex()
        .flex_col()
        .gap_10()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_size(px(20.)).font_weight(FontWeight::SEMIBOLD).child("Design tokens"))
                .child(div().text_size(text::MD).text_color(color::text_2()).child("crates/space-ui/src/theme")),
        )
        .child(section("Color").child(swatches))
        .child(section("Spacing").child(spacing))
        .child(section("Radius").child(radii))
        .child(section("Type").child(type_scale))
        .child(section("Motion").child(motion_rows))
}
