//! Manual regression check for GPUI's lazy Metal path attachments.
//! Start without paths, click to show them, resize, then hide/show them again.
use gpui::{prelude::*, *};

struct RendererCheck {
    show_paths: bool,
}

impl Render for RendererCheck {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x17202a))
            .text_color(rgb(0xffffff))
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child("Metal renderer: toggle paths, resize, then toggle again")
            .child(
                div()
                    .id("toggle-paths")
                    .p_3()
                    .bg(rgb(0x246c82))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_paths = !this.show_paths;
                        cx.notify();
                    }))
                    .child(if self.show_paths {
                        "Hide paths"
                    } else {
                        "Show paths"
                    }),
            )
            .when(self.show_paths, |view| {
                view.child(
                    canvas(
                        |_, _, _| (),
                        |bounds, _, window, _| {
                            let mut path = PathBuilder::fill();
                            path.move_to(bounds.origin + point(px(20.), px(150.)));
                            path.line_to(bounds.origin + point(px(140.), px(10.)));
                            path.line_to(bounds.origin + point(px(260.), px(150.)));
                            path.close();
                            window.paint_path(path.build().unwrap(), rgb(0x64d6ae));
                        },
                    )
                    .w_full()
                    .h(px(180.)),
                )
            })
    }
}

fn main() {
    Application::new().run(|cx| {
        cx.open_window(WindowOptions::default(), |_, cx| {
            cx.new(|_| RendererCheck { show_paths: false })
        })
        .unwrap();
        cx.activate(true);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}
