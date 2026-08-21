use gtk::prelude::*;
use sourceview5::prelude::*;

fn main() {
    gtk::init().unwrap();
    let provider = gtk::CssProvider::new();
    let css = hark::theme::Theme::load().to_css(&hark::config::UiThemeConfig::default());
    provider.load_from_string(&css);
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let window = gtk::ApplicationWindow::builder().build();
    window.add_css_class("hark-window");
    let view = sourceview5::View::new();
    view.add_css_class("hark-preview-code");
    view.set_show_line_numbers(true);
    let buf: sourceview5::Buffer = view.buffer().downcast::<sourceview5::Buffer>().unwrap();
    buf.set_text("fn main() {\n    let s = String::from(\"hi\");\n    println!(\"{}\", s);\n}");
    buf.set_highlight_syntax(true);
    buf.set_language(Some(
        &sourceview5::LanguageManager::default()
            .language("rust")
            .unwrap(),
    ));
    let mgr = sourceview5::StyleSchemeManager::default();
    let scheme = mgr.scheme("Adwaita-dark").expect("Adwaita-dark");
    buf.set_style_scheme(Some(&scheme));
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_child(Some(&view));
    window.set_child(Some(&scroll));
    window.set_size_request(720, 200);
    window.present();
    std::thread::sleep(std::time::Duration::from_secs(4));
}
