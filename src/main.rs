//! Linux App Manager — a private sideload/catalog manager for your own apps.
//!
//! Installed-vs-latest per app, one-click install / update / open / remove,
//! sources from GitHub releases / direct URLs / local folders. It manages
//! itself, exactly like the Android App Manager it's modelled on.

mod backends;
mod catalog;
mod config;
mod model;
mod sources;

// adw's prelude re-exports gtk's, so we don't import gtk::prelude separately.
use adw::prelude::*;
use catalog::{Entry, Status};
use model::{Kind, Origin, Source};
use gtk::{gio, glib};
use std::cell::RefCell;
use std::rc::Rc;

const APP_ID: &str = "com.procomputation.LinuxAppManager";

/// Widgets the whole UI shares.
struct Ui {
    window: adw::ApplicationWindow,
    list: gtk::ListBox,
    toasts: adw::ToastOverlay,
    /// Guards against overlapping refreshes.
    busy: RefCell<bool>,
}

fn main() -> glib::ExitCode {
    // Headless commands run without the GUI (and without fighting
    // GApplication's single-instance activation). No args → launch the app.
    if let Some(code) = run_cli() {
        return code;
    }

    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

/// Dispatch a headless command. Returns `Some(code)` when one was handled,
/// `None` (no args) to fall through to the GUI.
fn run_cli() -> Option<glib::ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first()?.as_str();
    Some(match cmd {
        "-h" | "--help" => {
            print_usage();
            glib::ExitCode::SUCCESS
        }
        "--list" => {
            let srcs = config::load_sources().unwrap_or_default();
            for e in catalog::build(&srcs) {
                println!("{:<28} {}", e.source.name, e.subtitle());
            }
            glib::ExitCode::SUCCESS
        }
        "--auto-update" => {
            let srcs = config::load_sources().unwrap_or_default();
            let r = catalog::auto_update(&srcs);
            for name in &r.updated {
                println!("updated: {name}");
            }
            for (name, e) in &r.failed {
                eprintln!("failed: {name}: {e}");
            }
            if r.updated.is_empty() && r.failed.is_empty() {
                println!("nothing to update");
            }
            if r.failed.is_empty() {
                glib::ExitCode::SUCCESS
            } else {
                glib::ExitCode::FAILURE
            }
        }
        "--import-official" => cli_import(sources::fetch_official()),
        "--import" => match args.get(1) {
            Some(path) => {
                let parsed = std::fs::read_to_string(path)
                    .map_err(anyhow::Error::from)
                    .and_then(|t| sources::parse_config(&t));
                cli_import(parsed)
            }
            None => {
                eprintln!("--import needs a file path");
                glib::ExitCode::FAILURE
            }
        },
        "--export" => match args.get(1) {
            Some(path) => {
                let srcs = config::load_sources().unwrap_or_default();
                match config::export_config(&srcs, std::path::Path::new(path)) {
                    Ok(()) => {
                        println!("Exported {} sources to {path}", srcs.len());
                        glib::ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("export failed: {e}");
                        glib::ExitCode::FAILURE
                    }
                }
            }
            None => {
                eprintln!("--export needs a file path");
                glib::ExitCode::FAILURE
            }
        },
        other => {
            eprintln!("unknown option: {other}\n");
            print_usage();
            glib::ExitCode::FAILURE
        }
    })
}

/// Merge parsed sources into the live list and save, reporting counts.
fn cli_import(incoming: anyhow::Result<Vec<Source>>) -> glib::ExitCode {
    let list = match incoming {
        Ok(l) => l,
        Err(e) => {
            eprintln!("import failed: {e}");
            return glib::ExitCode::FAILURE;
        }
    };
    let existing = config::load_sources().unwrap_or_default();
    let (merged, added, updated) = config::merge(&existing, list);
    match config::save_sources(&merged) {
        Ok(()) => {
            println!(
                "Imported: {added} added, {updated} updated ({} total)",
                merged.len()
            );
            glib::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("save failed: {e}");
            glib::ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    println!(
        "linux-app-manager — private sideload/catalog manager\n\n\
         USAGE:\n  \
         linux-app-manager                 launch the GUI\n  \
         linux-app-manager --list          print the catalog (installed vs latest)\n  \
         linux-app-manager --auto-update   install pending updates for auto-update apps\n  \
         linux-app-manager --import-official   merge the repo's official list\n  \
         linux-app-manager --import <file>     merge a config/sources file\n  \
         linux-app-manager --export <file>     write your list as a shareable config\n  \
         linux-app-manager --help          show this help"
    );
}

fn build_ui(app: &adw::Application) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list.add_css_class("boxed-list");

    let clamp = adw::Clamp::builder()
        .maximum_size(700)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .child(&list)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&clamp)
        .build();

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&scroller));

    let refresh_btn = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh_btn.set_tooltip_text(Some("Refresh"));
    let add_btn = gtk::Button::from_icon_name("list-add-symbolic");
    add_btn.set_tooltip_text(Some("Add app"));
    let menu_btn = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Import / export")
        .build();

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("App Manager", "")));
    header.pack_start(&refresh_btn);
    header.pack_end(&menu_btn);
    header.pack_end(&add_btn);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("App Manager")
        .default_width(560)
        .default_height(740)
        .content(&toolbar)
        .build();

    let ui = Rc::new(Ui {
        window: window.clone(),
        list,
        toasts,
        busy: RefCell::new(false),
    });

    let ui_btn = ui.clone();
    refresh_btn.connect_clicked(move |_| refresh(ui_btn.clone()));
    let ui_add = ui.clone();
    add_btn.connect_clicked(move |_| add_source_dialog(&ui_add));
    menu_btn.set_popover(Some(&import_export_menu(&ui)));

    window.present();
    refresh(ui.clone());
    run_auto_update(ui);
}

/// On startup, install pending updates for flagged apps, then refresh.
fn run_auto_update(ui: Rc<Ui>) {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let srcs = config::load_sources().unwrap_or_default();
        let _ = tx.send_blocking(catalog::auto_update(&srcs));
    });
    glib::spawn_future_local(async move {
        let Ok(result) = rx.recv().await else { return };
        for name in &result.updated {
            toast(&ui, &format!("Auto-updated {name} (restart to apply if it's this app)"));
        }
        for (name, e) in &result.failed {
            toast(&ui, &format!("Auto-update of {name} failed: {e}"));
        }
        if !result.updated.is_empty() {
            refresh(ui);
        }
    });
}

/// Popover with import/export actions for the header ▾ menu.
fn import_export_menu(ui: &Rc<Ui>) -> gtk::Popover {
    let pop = gtk::Popover::new();
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 2);
    vbox.set_margin_top(6);
    vbox.set_margin_bottom(6);
    vbox.set_margin_start(6);
    vbox.set_margin_end(6);

    let items: [(&str, fn(Rc<Ui>)); 3] = [
        ("Import official list", import_official),
        ("Import from file…", import_file),
        ("Export config…", export_file),
    ];
    for (label, action) in items {
        let btn = gtk::Button::builder()
            .label(label)
            .css_classes(["flat"])
            .halign(gtk::Align::Fill)
            .build();
        if let Some(child) = btn.child().and_downcast::<gtk::Label>() {
            child.set_xalign(0.0);
        }
        let ui = ui.clone();
        let pop2 = pop.clone();
        btn.connect_clicked(move |_| {
            pop2.popdown();
            action(ui.clone());
        });
        vbox.append(&btn);
    }
    pop.set_child(Some(&vbox));
    pop
}

/// Rebuild the catalog off-thread and repopulate the list.
fn refresh(ui: Rc<Ui>) {
    if *ui.busy.borrow() {
        return;
    }
    *ui.busy.borrow_mut() = true;

    clear(&ui.list);
    ui.list.append(&status_row("Loading…"));

    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let srcs = config::load_sources().unwrap_or_default();
        let _ = tx.send_blocking(catalog::build(&srcs));
    });

    glib::spawn_future_local(async move {
        let entries = rx.recv().await.unwrap_or_default();
        clear(&ui.list);
        if entries.is_empty() {
            ui.list.append(&status_row("No sources. Edit sources.json to add apps."));
        } else {
            for entry in entries {
                ui.list.append(&app_row(&ui, entry));
            }
        }
        *ui.busy.borrow_mut() = false;
    });
}

/// A single app's row: name, status subtitle, and action buttons.
fn app_row(ui: &Rc<Ui>, entry: Entry) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&entry.source.name)
        .subtitle(&entry.subtitle())
        .build();

    let status = entry.status();

    // Per-app auto-update toggle (off by default; on for the manager itself).
    let auto = gtk::Switch::builder()
        .active(entry.source.auto_update)
        .valign(gtk::Align::Center)
        .tooltip_text("Auto-update this app")
        .build();
    let id = entry.source.id.clone();
    let ui_sw = ui.clone();
    auto.connect_state_set(move |_, state| {
        if let Err(e) = config::set_auto_update(&id, state) {
            toast(&ui_sw, &format!("Save failed: {e}"));
        }
        glib::Propagation::Proceed
    });
    row.add_suffix(&auto);

    // Remove button, when something is installed.
    if matches!(status, Status::UpToDate | Status::UpdateAvailable | Status::Unknown) {
        let btn = gtk::Button::with_label("Remove");
        btn.set_valign(gtk::Align::Center);
        btn.add_css_class("flat");
        wire(&btn, ui, entry.clone(), Action::Remove);
        row.add_suffix(&btn);
    }

    // Primary action.
    let primary = match status {
        Status::NotInstalled => Some(("Install", Action::Install, true)),
        Status::UpdateAvailable => Some(("Update", Action::Update, true)),
        Status::UpToDate => Some(("Open", Action::Open, false)),
        Status::Unknown => Some(("Open", Action::Open, false)),
    };
    if let Some((label, action, suggested)) = primary {
        let btn = gtk::Button::with_label(label);
        btn.set_valign(gtk::Align::Center);
        if suggested {
            btn.add_css_class("suggested-action");
        }
        // Can't install/update without a resolved download asset.
        if matches!(action, Action::Install | Action::Update) && !entry.installable() {
            btn.set_sensitive(false);
            btn.set_tooltip_text(Some("No downloadable release asset for this app"));
        }
        wire(&btn, ui, entry.clone(), action);
        row.add_suffix(&btn);
    }

    row
}

#[derive(Clone, Copy)]
enum Action {
    Install,
    Update,
    Remove,
    Open,
}

fn wire(btn: &gtk::Button, ui: &Rc<Ui>, entry: Entry, action: Action) {
    let ui = ui.clone();
    btn.connect_clicked(move |b| {
        b.set_sensitive(false);
        do_action(ui.clone(), entry.clone(), action);
    });
}

/// Run a backend action off-thread, then refresh (Open is fire-and-forget).
fn do_action(ui: Rc<Ui>, entry: Entry, action: Action) {
    if let Action::Open = action {
        let _ = backends::open(&entry.source);
        return;
    }

    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let res = match action {
            Action::Install | Action::Update => match &entry.latest {
                Some(l) => backends::install(&entry.source, l),
                None => Ok(()),
            },
            Action::Remove => backends::remove(&entry.source),
            Action::Open => Ok(()),
        };
        let _ = tx.send_blocking(res);
    });

    glib::spawn_future_local(async move {
        if let Ok(Err(e)) = rx.recv().await {
            toast(&ui, &format!("Failed: {e}"));
        }
        refresh(ui);
    });
}

// --- small UI helpers ------------------------------------------------------

fn clear(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn status_row(text: &str) -> gtk::ListBoxRow {
    let label = gtk::Label::builder()
        .label(text)
        .margin_top(18)
        .margin_bottom(18)
        .build();
    label.add_css_class("dim-label");
    gtk::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .child(&label)
        .build()
}

fn toast(ui: &Rc<Ui>, text: &str) {
    ui.toasts.add_toast(adw::Toast::new(text));
}

// --- add source ------------------------------------------------------------

/// Dialog to add one app: name, GitHub repo, executable/package, and kind.
fn add_source_dialog(ui: &Rc<Ui>) {
    let dialog = adw::MessageDialog::new(Some(&ui.window), Some("Add app"), None);
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("add", "Add");
    dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("add"));
    dialog.set_close_response("cancel");

    let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let name_e = gtk::Entry::builder().placeholder_text("Name").build();
    let repo_e = gtk::Entry::builder()
        .placeholder_text("GitHub owner/repo")
        .build();
    let pkg_e = gtk::Entry::builder()
        .placeholder_text("Executable / package name")
        .build();
    let kind_dd = gtk::DropDown::from_strings(&["bin", "appimage", "deb"]);
    for w in [name_e.upcast_ref::<gtk::Widget>(), repo_e.upcast_ref(), pkg_e.upcast_ref()] {
        w.set_hexpand(true);
    }
    form.append(&name_e);
    form.append(&repo_e);
    form.append(&pkg_e);
    form.append(&kind_dd);
    dialog.set_extra_child(Some(&form));

    let ui = ui.clone();
    dialog.connect_response(None, move |_, resp| {
        if resp != "add" {
            return;
        }
        let name = name_e.text().trim().to_string();
        let repo = normalize_repo(&repo_e.text());
        let pkg = pkg_e.text().trim().to_string();
        if name.is_empty() || repo.is_empty() {
            toast(&ui, "Name and GitHub repo are required");
            return;
        }
        let kind = match kind_dd.selected() {
            1 => Kind::AppImage,
            2 => Kind::Deb,
            _ => Kind::Bin,
        };
        let id = if pkg.is_empty() { slug(&name) } else { pkg.clone() };
        let src = Source {
            id,
            name,
            description: None,
            kind,
            package: (!pkg.is_empty()).then_some(pkg),
            origin: Origin::Github { repo },
            auto_update: false,
        };
        apply_import(ui.clone(), vec![src]);
    });
    dialog.present();
}

// --- import / export -------------------------------------------------------

/// Merge sources into the live list, save, refresh, and report the result.
fn apply_import(ui: Rc<Ui>, incoming: Vec<Source>) {
    let existing = config::load_sources().unwrap_or_default();
    let (merged, added, updated) = config::merge(&existing, incoming);
    match config::save_sources(&merged) {
        Ok(()) => {
            toast(&ui, &format!("Imported · {added} added, {updated} updated"));
            refresh(ui);
        }
        Err(e) => toast(&ui, &format!("Save failed: {e}")),
    }
}

fn import_official(ui: Rc<Ui>) {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(sources::fetch_official());
    });
    glib::spawn_future_local(async move {
        match rx.recv().await {
            Ok(Ok(list)) => apply_import(ui, list),
            Ok(Err(e)) => toast(&ui, &format!("Import failed: {e}")),
            Err(_) => {}
        }
    });
}

fn import_file(ui: Rc<Ui>) {
    let dialog = gtk::FileDialog::builder().title("Import config").build();
    let win = ui.window.clone();
    dialog.open(Some(&win), gio::Cancellable::NONE, move |res| {
        let Ok(file) = res else { return };
        let Some(path) = file.path() else { return };
        match std::fs::read_to_string(&path) {
            Ok(text) => match sources::parse_config(&text) {
                Ok(list) => apply_import(ui.clone(), list),
                Err(e) => toast(&ui, &format!("Bad config: {e}")),
            },
            Err(e) => toast(&ui, &format!("Read failed: {e}")),
        }
    });
}

fn export_file(ui: Rc<Ui>) {
    let dialog = gtk::FileDialog::builder()
        .title("Export config")
        .initial_name("linux-app-manager-config.json")
        .build();
    let win = ui.window.clone();
    dialog.save(Some(&win), gio::Cancellable::NONE, move |res| {
        let Ok(file) = res else { return };
        let Some(path) = file.path() else { return };
        let srcs = config::load_sources().unwrap_or_default();
        match config::export_config(&srcs, &path) {
            Ok(()) => toast(&ui, "Config exported"),
            Err(e) => toast(&ui, &format!("Export failed: {e}")),
        }
    });
}

/// "https://github.com/owner/repo/" → "owner/repo".
fn normalize_repo(s: &str) -> String {
    s.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("github.com/")
        .trim_matches('/')
        .to_string()
}

/// A filesystem/id-safe slug from a display name.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        out.push(if c.is_ascii_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '-'
        });
    }
    out.trim_matches('-').to_string()
}
