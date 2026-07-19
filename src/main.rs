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
    nav: adw::NavigationView,
    /// Entries backing the list rows, indexed by row position (for row clicks).
    entries: RefCell<Vec<Entry>>,
    /// Guards against overlapping refreshes.
    busy: RefCell<bool>,
}

fn main() -> glib::ExitCode {
    // Headless commands run without the GUI (and without fighting
    // GApplication's single-instance activation). No args → launch the app.
    if let Some(code) = run_cli() {
        return code;
    }

    let mut builder = adw::Application::builder().application_id(APP_ID);
    // Test hook: run a second instance alongside a live one (skips
    // GApplication's single-instance activation) for smoke-testing.
    if std::env::var_os("LAM_NONUNIQUE").is_some() {
        builder = builder.flags(gio::ApplicationFlags::NON_UNIQUE);
    }
    let app = builder.build();
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
        "--follow-user" => match args.get(1) {
            Some(user) => cli_import(sources::follow_user(user)),
            None => {
                eprintln!("--follow-user needs a GitHub username");
                glib::ExitCode::FAILURE
            }
        },
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
         linux-app-manager --follow-user <u>   add a GitHub user's installable repos\n  \
         linux-app-manager --import-official   merge the repo's official list\n  \
         linux-app-manager --import <file>     merge a config/sources file\n  \
         linux-app-manager --export <file>     write your list as a shareable config\n  \
         linux-app-manager --help          show this help"
    );
}

fn build_ui(app: &adw::Application) {
    // Use the app-id icon (installed under hicolor/scalable/apps) for windows.
    gtk::Window::set_default_icon_name(APP_ID);

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

    // Root page of the navigation stack (the app list). Detail pages push on top.
    let root_toolbar = adw::ToolbarView::new();
    root_toolbar.add_top_bar(&header);
    root_toolbar.set_content(Some(&scroller));
    let root_page = adw::NavigationPage::builder()
        .title("App Manager")
        .tag("main")
        .child(&root_toolbar)
        .build();

    let nav = adw::NavigationView::new();
    nav.add(&root_page);

    // Toasts overlay the whole stack, so they show on detail pages too.
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&nav));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("App Manager")
        .default_width(560)
        .default_height(760)
        .content(&toasts)
        .build();

    let ui = Rc::new(Ui {
        window: window.clone(),
        list,
        toasts,
        nav,
        entries: RefCell::new(Vec::new()),
        busy: RefCell::new(false),
    });

    let ui_btn = ui.clone();
    refresh_btn.connect_clicked(move |_| refresh(ui_btn.clone()));
    let ui_add = ui.clone();
    add_btn.connect_clicked(move |_| source_dialog(&ui_add, None));
    menu_btn.set_popover(Some(&import_export_menu(&ui)));

    // Row click → push that app's detail page.
    let ui_row = ui.clone();
    ui.list.connect_row_activated(move |_, row| {
        let idx = row.index();
        if idx < 0 {
            return;
        }
        let entry = ui_row.entries.borrow().get(idx as usize).cloned();
        if let Some(entry) = entry {
            push_detail(&ui_row, entry);
        }
    });

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

    let items: [(&str, fn(Rc<Ui>)); 4] = [
        ("Follow GitHub user…", follow_user_dialog),
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

    ui.entries.borrow_mut().clear();
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
        // Keep entries in sync with row positions for click → detail lookups.
        *ui.entries.borrow_mut() = entries.clone();
        if entries.is_empty() {
            ui.list
                .append(&status_row("No sources. Add one with ＋ or import a list."));
        } else {
            for entry in entries {
                ui.list.append(&app_row(&ui, entry));
            }
        }
        *ui.busy.borrow_mut() = false;

        // Test hook: exercise detail-page construction + push for real.
        if std::env::var_os("LAM_TEST_DETAIL").is_some() {
            if let Some(e) = ui.entries.borrow().first().cloned() {
                let name = e.source.name.clone();
                push_detail(&ui, e);
                eprintln!("LAM_TEST_DETAIL: pushed detail for {name}");
            }
        }
    });
}

/// A single app's row: name, status subtitle, and action buttons.
fn app_row(ui: &Rc<Ui>, entry: Entry) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(&entry.source.name)
        .subtitle(&entry.subtitle())
        .activatable(true)
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

    // Uninstall button, when something is installed.
    if matches!(status, Status::UpToDate | Status::UpdateAvailable | Status::Unknown) {
        let btn = gtk::Button::with_label("Uninstall");
        btn.set_valign(gtk::Align::Center);
        btn.add_css_class("flat");
        btn.set_tooltip_text(Some("Uninstall from this machine"));
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

// --- detail page -----------------------------------------------------------

fn push_detail(ui: &Rc<Ui>, entry: Entry) {
    ui.nav.push(&build_detail_page(ui, entry));
}

/// A per-app page: description, details, release notes, and actions.
fn build_detail_page(ui: &Rc<Ui>, entry: Entry) -> adw::NavigationPage {
    let src = &entry.source;
    let prefs = adw::PreferencesPage::new();

    // Description (as the intro group's description text).
    if let Some(desc) = src.description.as_deref().filter(|d| !d.trim().is_empty()) {
        let head = adw::PreferencesGroup::builder().description(desc).build();
        prefs.add(&head);
    }

    // Details.
    let details = adw::PreferencesGroup::builder().title("Details").build();
    let add_row = |title: &str, value: &str| {
        let r = adw::ActionRow::builder().title(title).subtitle(value).build();
        r.set_subtitle_selectable(true);
        details.add(&r);
    };
    add_row("Status", status_text(entry.status()));
    add_row("Installed", entry.installed.as_deref().unwrap_or("—"));
    if let Some(l) = &entry.latest {
        add_row("Latest", &l.version);
        if let Some(sz) = l.size {
            add_row("Download size", &human_size(sz));
        }
    }
    add_row("Kind", kind_text(src.kind));
    add_row("Source", &origin_text(&src.origin));
    if let Some(p) = src.install_path.as_deref().filter(|p| !p.trim().is_empty()) {
        add_row("Path", p);
    }
    prefs.add(&details);

    // Release notes / changelog.
    if let Some(notes) = entry
        .latest
        .as_ref()
        .and_then(|l| l.notes.as_deref())
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        let g = adw::PreferencesGroup::builder().title("Release notes").build();
        let label = gtk::Label::builder()
            .label(notes)
            .wrap(true)
            .xalign(0.0)
            .selectable(true)
            .build();
        label.add_css_class("body");
        g.add(&label);
        prefs.add(&g);
    }

    // Actions.
    let actions = adw::PreferencesGroup::builder().title("Actions").build();
    let bx = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(6)
        .margin_bottom(6)
        .halign(gtk::Align::Start)
        .build();
    let status = entry.status();
    let primary = match status {
        Status::NotInstalled => Some(("Install", Action::Install, true)),
        Status::UpdateAvailable => Some(("Update", Action::Update, true)),
        Status::UpToDate | Status::Unknown => Some(("Open", Action::Open, false)),
    };
    if let Some((label, action, suggested)) = primary {
        let btn = gtk::Button::with_label(label);
        if suggested {
            btn.add_css_class("suggested-action");
        }
        if matches!(action, Action::Install | Action::Update) && !entry.installable() {
            btn.set_sensitive(false);
            btn.set_tooltip_text(Some("No downloadable release asset for this app"));
        }
        wire_detail(&btn, ui, entry.clone(), action);
        bx.append(&btn);
    }
    if matches!(status, Status::UpToDate | Status::UpdateAvailable | Status::Unknown) {
        let btn = gtk::Button::with_label("Uninstall");
        btn.add_css_class("destructive-action");
        btn.set_tooltip_text(Some("Remove the installed app from this machine"));
        wire_detail(&btn, ui, entry.clone(), Action::Remove);
        bx.append(&btn);
    }
    actions.add(&bx);

    // Manage-source row: edit this entry, or remove it from the list.
    let manage = adw::ActionRow::builder()
        .title("Source")
        .subtitle("Edit this entry or remove it from your list")
        .build();
    let edit_btn = gtk::Button::builder()
        .label("Edit…")
        .valign(gtk::Align::Center)
        .build();
    let ui_edit = ui.clone();
    let src_edit = src.clone();
    edit_btn.connect_clicked(move |_| {
        ui_edit.nav.pop();
        source_dialog(&ui_edit, Some(src_edit.clone()));
    });
    let remove_btn = gtk::Button::builder()
        .label("Remove from list")
        .valign(gtk::Align::Center)
        .build();
    remove_btn.add_css_class("destructive-action");
    let ui_rm = ui.clone();
    let id_rm = src.id.clone();
    remove_btn.connect_clicked(move |_| remove_from_list(ui_rm.clone(), &id_rm));
    manage.add_suffix(&edit_btn);
    manage.add_suffix(&remove_btn);
    actions.add(&manage);

    // Auto-update toggle.
    let auto_row = adw::ActionRow::builder()
        .title("Auto-update")
        .subtitle("Install updates automatically")
        .build();
    let auto = gtk::Switch::builder()
        .active(src.auto_update)
        .valign(gtk::Align::Center)
        .build();
    let id = src.id.clone();
    let ui_sw = ui.clone();
    auto.connect_state_set(move |_, state| {
        if let Err(e) = config::set_auto_update(&id, state) {
            toast(&ui_sw, &format!("Save failed: {e}"));
        }
        glib::Propagation::Proceed
    });
    auto_row.add_suffix(&auto);
    auto_row.set_activatable_widget(Some(&auto));
    actions.add(&auto_row);
    prefs.add(&actions);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&prefs));
    adw::NavigationPage::builder()
        .title(&src.name)
        .child(&toolbar)
        .build()
}

/// Detail-page action: run it, then return to the list (which refreshes).
fn wire_detail(btn: &gtk::Button, ui: &Rc<Ui>, entry: Entry, action: Action) {
    let ui = ui.clone();
    btn.connect_clicked(move |b| {
        b.set_sensitive(false);
        do_action(ui.clone(), entry.clone(), action);
        if !matches!(action, Action::Open) {
            ui.nav.pop();
        }
    });
}

fn status_text(s: Status) -> &'static str {
    match s {
        Status::NotInstalled => "Not installed",
        Status::UpToDate => "Up to date",
        Status::UpdateAvailable => "Update available",
        Status::Unknown => "Installed (latest unknown)",
    }
}

fn kind_text(k: Kind) -> &'static str {
    match k {
        Kind::Bin => "Executable (~/.local/bin)",
        Kind::AppImage => "AppImage (~/Applications)",
        Kind::Deb => "Debian package (apt)",
    }
}

fn origin_text(o: &Origin) -> String {
    match o {
        Origin::Github { repo } => format!("github.com/{repo}"),
        Origin::Url { url } => url.clone(),
        Origin::Local { path } => path.clone(),
    }
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
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

// --- add / edit source -----------------------------------------------------

/// Dialog to add a new app, or edit an existing one when `existing` is set
/// (fixes a wrong kind, repo, etc.). Name, GitHub repo, executable/package,
/// and kind.
fn source_dialog(ui: &Rc<Ui>, existing: Option<Source>) {
    let editing = existing.is_some();
    let dialog = adw::MessageDialog::new(
        Some(&ui.window),
        Some(if editing { "Edit app" } else { "Add app" }),
        None,
    );
    dialog.add_response("cancel", "Cancel");
    let ok = if editing { "save" } else { "add" };
    dialog.add_response(ok, if editing { "Save" } else { "Add" });
    dialog.set_response_appearance(ok, adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some(ok));
    dialog.set_close_response("cancel");

    let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let name_e = gtk::Entry::builder().placeholder_text("Name").build();
    let repo_e = gtk::Entry::builder()
        .placeholder_text("GitHub owner/repo")
        .build();
    let pkg_e = gtk::Entry::builder()
        .placeholder_text("Executable / package name")
        .build();
    let path_e = gtk::Entry::builder()
        .placeholder_text("bin path (optional, e.g. ~/App or /media/…/app)")
        .build();
    let kind_dd = gtk::DropDown::from_strings(&["bin", "appimage", "deb"]);
    for w in [
        name_e.upcast_ref::<gtk::Widget>(),
        repo_e.upcast_ref(),
        pkg_e.upcast_ref(),
        path_e.upcast_ref(),
    ] {
        w.set_hexpand(true);
    }

    // Prefill when editing.
    if let Some(s) = &existing {
        name_e.set_text(&s.name);
        if let Origin::Github { repo } = &s.origin {
            repo_e.set_text(repo);
        }
        pkg_e.set_text(s.package.as_deref().unwrap_or(""));
        path_e.set_text(s.install_path.as_deref().unwrap_or(""));
        kind_dd.set_selected(match s.kind {
            Kind::Bin => 0,
            Kind::AppImage => 1,
            Kind::Deb => 2,
        });
    }

    form.append(&name_e);
    form.append(&repo_e);
    form.append(&pkg_e);
    form.append(&path_e);
    form.append(&kind_dd);
    dialog.set_extra_child(Some(&form));

    let ui = ui.clone();
    dialog.connect_response(None, move |_, resp| {
        if resp != ok {
            return;
        }
        let name = name_e.text().trim().to_string();
        let repo = normalize_repo(&repo_e.text());
        let pkg = pkg_e.text().trim().to_string();
        let path = path_e.text().trim().to_string();
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
            id: id.clone(),
            name,
            // Preserve description + auto-update flag across an edit.
            description: existing.as_ref().and_then(|e| e.description.clone()),
            kind,
            package: (!pkg.is_empty()).then_some(pkg),
            install_path: (!path.is_empty()).then_some(path),
            origin: Origin::Github { repo },
            auto_update: existing.as_ref().map(|e| e.auto_update).unwrap_or(false),
        };
        // If editing changed the id, drop the old entry so we don't dup it.
        if let Some(old) = &existing {
            if old.id != id {
                let _ = config::remove_source(&old.id);
            }
        }
        apply_import(ui.clone(), vec![src]);
    });
    dialog.present();
}

/// Ask for a GitHub username, then add all their installable repos.
fn follow_user_dialog(ui: Rc<Ui>) {
    let dialog = adw::MessageDialog::new(
        Some(&ui.window),
        Some("Follow a GitHub user"),
        Some("Adds every repo of theirs that has a release installable on this machine."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("go", "Add apps");
    dialog.set_response_appearance("go", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("go"));
    dialog.set_close_response("cancel");

    let entry = gtk::Entry::builder()
        .placeholder_text("GitHub username")
        .activates_default(true)
        .build();
    dialog.set_extra_child(Some(&entry));

    dialog.connect_response(None, move |_, resp| {
        if resp != "go" {
            return;
        }
        let user = entry.text().trim().to_string();
        if user.is_empty() {
            toast(&ui, "Enter a username");
            return;
        }
        toast(&ui, &format!("Scanning {user}'s repos…"));
        let ui2 = ui.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(sources::follow_user(&user));
        });
        glib::spawn_future_local(async move {
            match rx.recv().await {
                Ok(Ok(list)) if list.is_empty() => toast(&ui2, "No installable repos found"),
                Ok(Ok(list)) => apply_import(ui2, list),
                Ok(Err(e)) => toast(&ui2, &format!("Follow failed: {e}")),
                Err(_) => {}
            }
        });
    });
    dialog.present();
}

/// Delete a source from the list (not the installed app), then return + refresh.
fn remove_from_list(ui: Rc<Ui>, id: &str) {
    match config::remove_source(id) {
        Ok(()) => {
            toast(&ui, "Removed from list");
            ui.nav.pop();
            refresh(ui);
        }
        Err(e) => toast(&ui, &format!("Failed: {e}")),
    }
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
