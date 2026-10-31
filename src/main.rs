//! Linux App Manager — a private sideload/catalog manager for your own apps.
//!
//! Installed-vs-latest per app, one-click install / update / open / remove,
//! sources from GitHub releases / direct URLs / local folders. It manages
//! itself, exactly like the Android App Manager it's modelled on.

mod api;
mod backends;
mod catalog;
mod config;
mod model;
mod sources;

// adw's prelude re-exports gtk's, so we don't import gtk::prelude separately.
use adw::prelude::*;
use api::{ApiRequest, ApiResponse};
use catalog::{Entry, Status};
use model::{Kind, Origin, Source};
use gtk::{gio, glib};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use serde_json::{json, Value};

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
    /// App ids with an install/update/removal running. `wire` greys the button
    /// that was clicked, but that is not enough on its own: the row and the
    /// detail page each build their own button, and a Refresh mid-download
    /// rebuilds the row with a fresh, sensitive one. Tracking the id instead of
    /// the widget survives all of that.
    inflight: RefCell<HashSet<String>>,
    /// The running local API, if it is switched on. Dropping it stops it.
    api: RefCell<Option<api::Server>>,
    api_tx: async_channel::Sender<ApiRequest>,
    api_key: RefCell<String>,
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
        // The key is needed to call the API at all, and asking the API for it
        // is not a plan, so the CLI prints it (creating it on first use).
        "--api-key" => match api::load_or_create_key() {
            Some(key) => {
                println!("{key}");
                glib::ExitCode::SUCCESS
            }
            None => {
                eprintln!("could not read or create the API key file");
                glib::ExitCode::FAILURE
            }
        },
        "--install-self" => match backends::install_self() {
            Ok(path) => {
                println!("Installed App Manager to {}", path.display());
                glib::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("install failed: {e}");
                glib::ExitCode::FAILURE
            }
        },
        "--install" => match args.get(1) {
            Some(id) => cli_install(id),
            None => {
                eprintln!("--install needs an app id or name (see --list)");
                glib::ExitCode::FAILURE
            }
        },
        "--add" => match args.get(1) {
            Some(input) => cli_import(sources::resolve_repo(input).map(|s| vec![s])),
            None => {
                eprintln!("--add needs a GitHub repo or URL");
                glib::ExitCode::FAILURE
            }
        },
        "--follow-user" => match args.get(1) {
            Some(user) => {
                // Remember the account so new repos get auto-discovered later.
                let _ = config::add_follow(user);
                cli_import(sources::follow_user(user))
            }
            None => {
                eprintln!("--follow-user needs a GitHub username");
                glib::ExitCode::FAILURE
            }
        },
        "--discover" => {
            // Re-scan every followed account for new installable repos (the same
            // pass the GUI runs on startup/refresh).
            let follows = config::load_follows();
            if follows.is_empty() {
                eprintln!("no followed users — add one with --follow-user <u>");
                glib::ExitCode::FAILURE
            } else {
                cli_import(Ok(sources::discover_follows(&follows)))
            }
        }
        "--import-official" => cli_import(sources::import_official()),
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

/// Install (or update) one app by id or name, headless. Mirrors the GUI's
/// Install/Update button so it can be scripted and tested.
fn cli_install(needle: &str) -> glib::ExitCode {
    let srcs = config::load_sources().unwrap_or_default();
    let entries = catalog::build(&srcs);
    let Some(entry) = entries.iter().find(|e| {
        e.source.id.eq_ignore_ascii_case(needle) || e.source.name.eq_ignore_ascii_case(needle)
    }) else {
        eprintln!("no app matching '{needle}' — run --list to see ids");
        return glib::ExitCode::FAILURE;
    };
    let Some(latest) = &entry.latest else {
        eprintln!("{}: couldn't resolve a latest release to install", entry.source.name);
        return glib::ExitCode::FAILURE;
    };
    if !entry.installable() {
        eprintln!("{}: no downloadable release asset for this host", entry.source.name);
        return glib::ExitCode::FAILURE;
    }
    match backends::install(&entry.source, latest) {
        Ok(()) => {
            println!("Installed {} {}", entry.source.name, latest.version);
            if entry.source.cli {
                let pkg = entry.source.package_name();
                println!("Terminal app — open a new terminal and run: {pkg}");
            }
            glib::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("install failed: {e}");
            glib::ExitCode::FAILURE
        }
    }
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
         linux-app-manager --add <repo>    add one GitHub repo/URL (auto-detected)\n  \
         linux-app-manager --install <id>  install/update one app by id or name\n  \
         linux-app-manager --install-self  install THIS copy for this user (+icon, menu entry)\n  \
         linux-app-manager --auto-update   install pending updates for auto-update apps\n  \
         linux-app-manager --follow-user <u>   follow a GitHub user's installable repos\n  \
         linux-app-manager --discover      re-scan followed users for new repos\n  \
         linux-app-manager --import-official   merge the repo's official list\n  \
         linux-app-manager --import <file>     merge a config/sources file\n  \
         linux-app-manager --export <file>     write your list as a shareable config\n  \
         linux-app-manager --api-key       print the local API key\n  \
         linux-app-manager --help          show this help"
    );
}

fn build_ui(app: &adw::Application) {
    // A second launch activates the running instance. Building a second window
    // would also start a second refresh + auto-update pass and try to bind the
    // API port again, so bring the existing window forward instead.
    if let Some(win) = app.active_window() {
        win.present();
        return;
    }
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

    let (api_tx, api_rx) = async_channel::unbounded::<ApiRequest>();
    let ui = Rc::new(Ui {
        window: window.clone(),
        list,
        toasts,
        nav,
        entries: RefCell::new(Vec::new()),
        busy: RefCell::new(false),
        inflight: RefCell::new(HashSet::new()),
        api: RefCell::new(None),
        api_tx,
        api_key: RefCell::new(api::load_or_create_key().unwrap_or_default()),
    });

    let ui_btn = ui.clone();
    refresh_btn.connect_clicked(move |_| {
        // Manual refresh also re-scans followed accounts for new repos.
        discover_follows(ui_btn.clone());
        refresh(ui_btn.clone());
    });
    let ui_add = ui.clone();
    add_btn.connect_clicked(move |_| add_url_dialog(&ui_add));
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
    let settings = config::load_settings();
    if settings.api_enabled {
        if let Err(e) = rebind_api(&ui, settings.api_port) {
            toast(&ui, &format!("Local API could not listen on port {}: {e}", settings.api_port));
        }
    }
    serve_api(&ui, api_rx);
    refresh(ui.clone());
    discover_follows(ui.clone());
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

/// Re-scan followed GitHub accounts for newly-published installable repos and
/// merge any into the list. Network-heavy (a release lookup per repo per
/// followed user) so it runs off the UI thread; a no-op when nothing is
/// followed. Called on startup and when Refresh is pressed.
fn discover_follows(ui: Rc<Ui>) {
    let follows = config::load_follows();
    if follows.is_empty() {
        return;
    }
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(sources::discover_follows(&follows));
    });
    glib::spawn_future_local(async move {
        let Ok(found) = rx.recv().await else { return };
        if found.is_empty() {
            return;
        }
        let existing = config::load_sources().unwrap_or_default();
        let (merged, added, _) = config::merge(&existing, found);
        if added > 0 && config::save_sources(&merged).is_ok() {
            toast(&ui, &format!("Found {added} new app(s) from followed users"));
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

    let items: [(&str, fn(Rc<Ui>)); 5] = [
        ("Follow GitHub user…", follow_user_dialog),
        ("Import official list", import_official),
        ("Import from file…", import_file),
        ("Export config…", export_file),
        ("Local API…", api_dialog),
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

    // Primary action. Our own entry gets a self-install button (copies the
    // running binary) instead of an Open/download action.
    let is_self = entry.source.id == APP_ID;
    if is_self {
        if let Some(btn) = self_action_button(ui, &entry, status) {
            btn.set_valign(gtk::Align::Center);
            row.add_suffix(&btn);
        }
    } else if let Some((label, action, suggested)) = normal_primary(status, entry.source.cli, entry.installable()) {
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

/// Install/Update/Open label for a normal (non-self) app. Terminal apps get no
/// Open button — there is no window to open; they're run from a shell.
///
/// "Installed · version unknown" means the app got here some way App Manager
/// didn't record (hand-copied, pre-placed, apt), so there's no version sidecar
/// and nothing to read out of a bare binary/AppImage. When a downloadable
/// release exists, the primary becomes **Reinstall**: it fetches the current
/// release and records its version, turning the dead-end "unknown" into a
/// one-click fix. (Open returns as soon as the version is known.)
fn normal_primary(status: Status, cli: bool, installable: bool) -> Option<(&'static str, Action, bool)> {
    match status {
        Status::NotInstalled => Some(("Install", Action::Install, true)),
        Status::UpdateAvailable => Some(("Update", Action::Update, true)),
        Status::Unknown if installable => Some(("Reinstall", Action::Update, true)),
        Status::UpToDate | Status::Unknown if cli => None,
        Status::UpToDate | Status::Unknown => Some(("Open", Action::Open, false)),
    }
}

/// The action button for App Manager's own entry: install THIS running copy
/// (whole AppImage → ~/Applications, loose binary → ~/.local/bin) when we're
/// not already the installed one; otherwise a normal download-update if a
/// newer release is out; else nothing.
fn self_action_button(ui: &Rc<Ui>, entry: &Entry, status: Status) -> Option<gtk::Button> {
    if let Some(label) = self_install_label() {
        let btn = gtk::Button::with_label(label);
        btn.add_css_class("suggested-action");
        let tip = if backends::running_appimage().is_some() {
            "Install to your app menu (all libraries included)"
        } else {
            "Install this running copy to ~/.local/bin"
        };
        btn.set_tooltip_text(Some(tip));
        let ui = ui.clone();
        btn.connect_clicked(move |b| {
            b.set_sensitive(false);
            do_self_install(ui.clone());
        });
        Some(btn)
    } else if matches!(status, Status::UpdateAvailable) && entry.installable() {
        let btn = gtk::Button::with_label("Update");
        btn.add_css_class("suggested-action");
        let ui = ui.clone();
        let entry = entry.clone();
        btn.connect_clicked(move |b| {
            b.set_sensitive(false);
            do_action(ui.clone(), entry.clone(), Action::Update);
        });
        Some(btn)
    } else {
        None
    }
}

/// Label for the self-install button, or None when we're already running the
/// installed copy (the extracted bundle, or the ~/.local/bin binary).
fn self_install_label() -> Option<&'static str> {
    let installed = config::localbin_dir().join("linux-app-manager");
    if backends::running_appimage().is_some() {
        // Running straight off the .AppImage file (USB / download) — always
        // offer to install/refresh the on-disk bundle.
        return Some(if installed.exists() { "Reinstall this copy" } else { "Install" });
    }
    let running = std::env::current_exe().ok()?;
    if running.starts_with(config::self_bundle_dir()) {
        return None; // we ARE the installed bundle
    }
    let running_c = std::fs::canonicalize(&running).ok();
    let installed_c = std::fs::canonicalize(&installed).ok();
    if running_c.is_some() && running_c == installed_c {
        None
    } else if installed.exists() {
        Some("Reinstall this copy")
    } else {
        Some("Install")
    }
}

/// Install the running copy (AppImage or loose binary) off-thread.
fn do_self_install(ui: Rc<Ui>) {
    glib::spawn_future_local(perform_self_install(ui));
}

/// The self-install itself; the API awaits the outcome, a click ignores it.
async fn perform_self_install(ui: Rc<Ui>) -> Result<String, String> {
    match background(backends::install_self).await {
        Some(Ok(path)) => {
            toast(&ui, &format!("Installed to {} — launch it from your app menu", path.display()));
            refresh(ui);
            Ok(path.display().to_string())
        }
        Some(Err(e)) => {
            toast(&ui, &format!("Install failed: {e}"));
            Err(e.to_string())
        }
        None => Err("the install was interrupted".into()),
    }
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
    if src.cli {
        add_row(
            "Run from terminal",
            &format!("{}  (~/.local/bin must be on your PATH)", src.package_name()),
        );
    }
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
    let is_self = entry.source.id == APP_ID;
    if is_self {
        if let Some(btn) = self_action_button(ui, &entry, status) {
            bx.append(&btn);
        }
    } else if let Some((label, action, suggested)) = normal_primary(status, entry.source.cli, entry.installable()) {
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
    grey_while_inflight(btn, ui, &entry.source.id);
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
        Kind::AppImage => "AppImage (unpacked into the app menu)",
        Kind::Deb => "Debian package (apt)",
        Kind::Tar => "Tarball (~/.local/bin)",
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
    grey_while_inflight(btn, ui, &entry.source.id);
    let ui = ui.clone();
    btn.connect_clicked(move |b| {
        b.set_sensitive(false);
        do_action(ui.clone(), entry.clone(), action);
    });
}

/// These buttons are rebuilt from scratch on every refresh, so a button that
/// was greyed by its own click comes back sensitive if the user hits Refresh
/// mid-download. Re-apply the disabled state from the app id instead.
fn grey_while_inflight(btn: &gtk::Button, ui: &Rc<Ui>, id: &str) {
    if ui.inflight.borrow().contains(id) {
        btn.set_sensitive(false);
        btn.set_tooltip_text(Some("Already working on this app…"));
    }
}

/// Run a backend action off-thread, then refresh (Open is fire-and-forget).
fn do_action(ui: Rc<Ui>, entry: Entry, action: Action) {
    glib::spawn_future_local(async move {
        let _ = perform(ui, entry, action).await;
    });
}

/// Why an action did not happen: `busy` when the app already has one running
/// (the API answers 409 for that), otherwise it was tried and failed.
struct ActionError {
    busy: bool,
    message: String,
}

/// The action behind every Install/Update/Reinstall/Uninstall/Open — a click
/// and an API call both land here, so they share the one-per-app guard, the
/// progress toast and the refresh. Resolves to the success message.
async fn perform(ui: Rc<Ui>, entry: Entry, action: Action) -> Result<String, ActionError> {
    if let Action::Open = action {
        return backends::open(&entry.source)
            .map(|()| format!("Opened {}", entry.source.name))
            .map_err(|e| ActionError { busy: false, message: e.to_string() });
    }

    // One action per app at a time. Without this a second click starts a second
    // download to the same cache path, which breaks the first one with ETXTBSY
    // ("Text file busy") when it tries to unpack — the download that was
    // already working is the one that dies.
    let id = entry.source.id.clone();
    if !ui.inflight.borrow_mut().insert(id.clone()) {
        let message = format!("{} is already working — hang on", entry.source.name);
        toast(&ui, &message);
        return Err(ActionError { busy: true, message });
    }

    // A terminal app has nothing to click afterwards — tell the user how to
    // run it once the install lands.
    let cli_hint = (entry.source.cli && matches!(action, Action::Install | Action::Update))
        .then(|| entry.source.package_name().to_string());
    // A positive confirmation for the toast, so a successful non-cli install
    // isn't silent (previously only the row changed on refresh — invisible if
    // the refresh then failed to resolve versions).
    let name = entry.source.name.clone();
    let done = match action {
        Action::Install => "Installed",
        Action::Update => "Updated",
        Action::Remove => "Removed",
        Action::Open => "",
    };

    // A release AppImage can be well over 100 MB, and until now the only sign
    // anything was happening was the clicked button going grey — which reads as
    // "nothing happened", and invites the second click that used to break it.
    // timeout(0) pins the toast up until the action reports back.
    let progress = adw::Toast::new(&match action {
        Action::Install => format!("Downloading {name}… this can take a few minutes"),
        Action::Update => format!("Downloading the {name} update… this can take a few minutes"),
        Action::Remove => format!("Removing {name}…"),
        Action::Open => String::new(),
    });
    progress.set_timeout(0);
    ui.toasts.add_toast(progress.clone());

    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let res = match action {
            Action::Install | Action::Update => match &entry.latest {
                Some(l) => backends::install(&entry.source, l),
                // No resolved release is a real failure (GitHub unreachable /
                // rate-limited), not a silent no-op — say so.
                None => Err(anyhow::anyhow!(
                    "couldn't reach GitHub to resolve a release — check your \
                     connection, or you may be rate-limited (try again later)"
                )),
            },
            Action::Remove => backends::remove(&entry.source),
            Action::Open => Ok(()),
        };
        let _ = tx.send_blocking(res);
    });

    let outcome = rx.recv().await;
    progress.dismiss();
    ui.inflight.borrow_mut().remove(&id);
    let result = match outcome {
        Ok(Err(e)) => Err(ActionError { busy: false, message: format!("Failed: {e}") }),
        Ok(Ok(())) => Ok(match cli_hint {
            Some(pkg) => format!("Installed. '{pkg}' is a terminal app — open a NEW terminal and run: {pkg}"),
            None => format!("{done} {name}"),
        }),
        // The worker thread died without reporting (dropped sender) — never
        // leave the click with no feedback at all.
        Err(_) => Err(ActionError { busy: false, message: "Action interrupted — please try again".into() }),
    };
    match &result {
        Ok(m) => toast(&ui, m),
        Err(e) => toast(&ui, &e.message),
    }
    refresh(ui);
    result
}

/// Run blocking work (network, dpkg, disk) on a worker thread and await it from
/// the main loop. `None` only if the worker died without answering.
async fn background<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(f());
    });
    rx.recv().await.ok()
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

/// Quick add: paste a GitHub repo/URL and let App Manager fill in the rest.
/// "Manual…" falls through to the full form for edge cases.
fn add_url_dialog(ui: &Rc<Ui>) {
    let dialog = adw::MessageDialog::new(
        Some(&ui.window),
        Some("Add app"),
        Some("Paste a GitHub repo or URL — App Manager fills in the rest."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("manual", "Manual…");
    dialog.add_response("add", "Add");
    dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("add"));
    dialog.set_close_response("cancel");

    let entry = gtk::Entry::builder()
        .placeholder_text("github.com/owner/repo")
        .activates_default(true)
        .hexpand(true)
        .build();
    dialog.set_extra_child(Some(&entry));

    let ui = ui.clone();
    dialog.connect_response(None, move |_, resp| match resp {
        "manual" => source_dialog(&ui, None),
        "add" => {
            let input = entry.text().trim().to_string();
            if input.is_empty() {
                toast(&ui, "Paste a GitHub repo or URL");
                return;
            }
            toast(&ui, "Resolving…");
            let ui2 = ui.clone();
            let (tx, rx) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let _ = tx.send_blocking(sources::resolve_repo(&input));
            });
            glib::spawn_future_local(async move {
                match rx.recv().await {
                    Ok(Ok(src)) => apply_import(ui2, vec![src]),
                    Ok(Err(e)) => toast(&ui2, &format!("Couldn't add: {e}")),
                    Err(_) => {}
                }
            });
        }
        _ => {}
    });
    dialog.present();
}

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
    let kind_dd = gtk::DropDown::from_strings(&["bin", "appimage", "deb", "tar"]);
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
            Kind::Tar => 3,
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
            3 => Kind::Tar,
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
            cli: false,
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
        // Subscribe: remembered so new repos of theirs are auto-discovered on
        // later startups/refreshes, not just imported this once.
        let _ = config::add_follow(&user);
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
        let _ = tx.send_blocking(sources::import_official());
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

// --- local API -------------------------------------------------------------

/// ▾ menu → Local API: the switch, the port and the key. The key is shown and
/// editable — a key the user cannot read is one they cannot hand to the thing
/// that needs it — and can be replaced with their own or regenerated.
fn api_dialog(ui: Rc<Ui>) {
    let settings = config::load_settings();
    let dialog = adw::PreferencesDialog::builder().title("Local API").build();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder()
        .description(
            "Lets other programs on this computer do anything you can do here: \
             list, install, update, remove and open apps, and manage your list. \
             Loopback only (127.0.0.1); every request needs the key.",
        )
        .build();

    let switch = adw::SwitchRow::builder()
        .title("Enable local API")
        .subtitle(api_hint(ui.api.borrow().as_ref().map(|s| s.port())))
        .active(settings.api_enabled)
        .build();
    let port = adw::SpinRow::with_range(1024.0, 65535.0, 1.0);
    port.set_title("Port");
    port.set_value(settings.api_port as f64);

    let key = adw::EntryRow::builder()
        .title("API key (edit and press ✓ to use your own)")
        .show_apply_button(true)
        .build();
    key.set_text(&ui.api_key.borrow());
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy the key")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let regen = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Issue a new random key — anything using the old one stops working")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    key.add_suffix(&copy);
    key.add_suffix(&regen);

    copy.connect_clicked({
        let key = key.clone();
        let ui = ui.clone();
        move |_| {
            key.clipboard().set_text(&key.text());
            toast(&ui, "API key copied");
        }
    });
    regen.connect_clicked({
        let (ui, key, switch) = (ui.clone(), key.clone(), switch.clone());
        move |_| match api::regenerate_key() {
            Some(k) => {
                key.set_text(&k);
                apply_new_key(&ui, k, &switch);
            }
            None => toast(&ui, "Could not write the key file"),
        }
    });
    key.connect_apply({
        let (ui, switch) = (ui.clone(), switch.clone());
        move |row| match api::set_key(&row.text()) {
            Ok(k) => apply_new_key(&ui, k, &switch),
            Err(e) => {
                toast(&ui, &format!("Key not changed: {e}"));
                row.set_text(&ui.api_key.borrow());
            }
        }
    });
    switch.connect_active_notify({
        let (ui, port) = (ui.clone(), port.clone());
        move |sw| {
            let p = port.value() as u16;
            set_api(&ui, sw.is_active(), p);
            sw.set_subtitle(&api_hint(ui.api.borrow().as_ref().map(|s| s.port())));
        }
    });
    port.connect_value_notify({
        let (ui, switch) = (ui.clone(), switch.clone());
        move |port| {
            let p = port.value() as u16;
            set_api(&ui, switch.is_active(), p);
            switch.set_subtitle(&api_hint(ui.api.borrow().as_ref().map(|s| s.port())));
        }
    });

    group.add(&switch);
    group.add(&port);
    group.add(&key);
    page.add(&group);
    dialog.add(&page);
    dialog.present(Some(&ui.window));
}

fn api_hint(port: Option<u16>) -> String {
    match port {
        Some(p) => format!("Listening on http://127.0.0.1:{p}/api — see GET /api/docs"),
        None => "Off — nothing is listening".to_string(),
    }
}

/// The running server captured the old key, so it has to come back up on the
/// new one or the key on screen would be a lie.
fn apply_new_key(ui: &Rc<Ui>, key: String, switch: &adw::SwitchRow) {
    *ui.api_key.borrow_mut() = key;
    let running = ui.api.borrow().as_ref().map(|s| s.port());
    if let Some(port) = running {
        if let Err(e) = rebind_api(ui, port) {
            toast(ui, &format!("Local API could not restart on port {port}: {e}"));
        }
    }
    switch.set_subtitle(&api_hint(ui.api.borrow().as_ref().map(|s| s.port())));
    toast(ui, "API key changed — the old key no longer works");
}

/// Switch the API on (on `port`) or off, and remember the choice. A failure to
/// listen is said out loud: a silently dead API wastes an afternoon.
fn set_api(ui: &Rc<Ui>, on: bool, port: u16) {
    if on {
        if let Err(e) = rebind_api(ui, port) {
            toast(ui, &format!("Local API could not listen on port {port}: {e}"));
        }
    } else {
        // Dropping the Server is what stops the listener.
        *ui.api.borrow_mut() = None;
    }
    if let Err(e) = config::save_settings(&config::Settings { api_enabled: on, api_port: port }) {
        toast(ui, &format!("Save failed: {e}"));
    }
}

/// (Re)start the listener on `port` with the current key. On a new port the
/// new listener is bound *before* the old one is dropped, so a port that turns
/// out to be taken leaves the API up where it was instead of leaving it dead.
/// On the same port the old one has to go first — two cannot hold it.
fn rebind_api(ui: &Rc<Ui>, port: u16) -> std::io::Result<()> {
    if ui.api.borrow().as_ref().map(|s| s.port()) == Some(port) {
        *ui.api.borrow_mut() = None;
    }
    let server = api::start(port, ui.api_key.borrow().clone(), ui.api_tx.clone())?;
    *ui.api.borrow_mut() = Some(server);
    Ok(())
}

/// Answer API requests on the main loop. Each request gets its own task, so a
/// five-minute install does not hold up a status call behind it.
fn serve_api(ui: &Rc<Ui>, rx: async_channel::Receiver<ApiRequest>) {
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        while let Ok(request) = rx.recv().await {
            let ui = ui.clone();
            glib::spawn_future_local(async move {
                let response = dispatch(ui, &request).await;
                // A send error only means the caller gave up first.
                let _ = request.reply.send(response).await;
            });
        }
    });
}

fn status_key(s: Status) -> &'static str {
    match s {
        Status::NotInstalled => "not_installed",
        Status::UpToDate => "up_to_date",
        Status::UpdateAvailable => "update_available",
        Status::Unknown => "unknown",
    }
}

fn entry_json(ui: &Rc<Ui>, e: &Entry) -> Value {
    let src = &e.source;
    json!({
        "id": src.id,
        "name": src.name,
        "description": src.description,
        "kind": src.kind,
        "status": status_key(e.status()),
        "summary": e.subtitle(),
        "installed": e.installed,
        "latest": e.latest.as_ref().map(|l| l.version.clone()),
        "download_size": e.latest.as_ref().and_then(|l| l.size),
        "release_notes": e.latest.as_ref().and_then(|l| l.notes.clone()),
        "installable": e.installable(),
        "auto_update": src.auto_update,
        "cli": src.cli,
        "origin": src.origin,
        "package": src.package,
        "install_path": src.install_path,
        "busy": ui.inflight.borrow().contains(&src.id),
        "is_self": src.id == APP_ID,
    })
}

/// One source, resolved fresh: latest release + what's installed. Unlike
/// `catalog::build` it never writes the source list, so it is safe to call on
/// a single entry (build would persist a one-entry list after a kind heal).
fn resolve_entry(src: Source) -> Entry {
    let (latest, corrected) = sources::resolve_latest(&src);
    let mut source = src;
    if let Some(kind) = corrected {
        source.kind = kind;
    }
    Entry { installed: backends::detect_installed(&source), source, latest }
}

/// A source by id, or failing that by display name (both case-insensitive).
fn find_source(needle: &str) -> Option<Source> {
    let srcs = config::load_sources().unwrap_or_default();
    srcs.iter()
        .find(|s| s.id.eq_ignore_ascii_case(needle))
        .or_else(|| srcs.iter().find(|s| s.name.eq_ignore_ascii_case(needle)))
        .cloned()
}

fn no_app(needle: &str) -> ApiResponse {
    ApiResponse::error(404, &format!("no app matching '{needle}' — GET /api/apps lists them"))
}

/// Merge into the saved list and refresh the window — the API's apply_import.
fn merge_and_save(ui: &Rc<Ui>, incoming: Vec<Source>) -> ApiResponse {
    let existing = config::load_sources().unwrap_or_default();
    let (merged, added, updated) = config::merge(&existing, incoming);
    match config::save_sources(&merged) {
        Ok(()) => {
            refresh(ui.clone());
            ApiResponse::ok(json!({ "ok": true, "added": added, "updated": updated, "total": merged.len() }))
        }
        Err(e) => ApiResponse::error(500, &format!("save failed: {e}")),
    }
}

fn index() -> Value {
    json!({
        "ok": true,
        "name": "linux-app-manager",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": {
            "GET    /api/docs": "the full reference, as Markdown — ?section= narrows it",
            "GET    /api/status": "version, running copy, actions in flight, API port",
            "GET    /api/apps": "every app: installed vs latest (hits GitHub — a few seconds)",
            "POST   /api/apps": "{repo} quick-add a GitHub repo/URL, or {source: {...}} a full entry",
            "GET    /api/apps/{id}": "one app (id or name)",
            "PATCH  /api/apps/{id}": "{name?, description?, kind?, package?, install_path?, repo?, url?, path?, auto_update?, cli?}",
            "DELETE /api/apps/{id}": "remove from the list (does not uninstall)",
            "POST   /api/apps/{id}/install": "install — waits until done",
            "POST   /api/apps/{id}/update": "update — waits until done",
            "POST   /api/apps/{id}/reinstall": "download and install the latest again",
            "POST   /api/apps/{id}/uninstall": "remove the installed app from this machine",
            "POST   /api/apps/{id}/open": "launch it",
            "POST   /api/apps/{id}/auto-update": "{on: bool}",
            "GET    /api/sources": "the raw saved list",
            "GET    /api/follows": "followed GitHub users",
            "POST   /api/follow": "{user} — follow and add their installable repos",
            "POST   /api/discover": "re-scan followed users for new repos",
            "POST   /api/import-official": "merge the curated official list",
            "POST   /api/import": "{config: <document or JSON text>} or {path}",
            "GET    /api/export": "your list as a shareable config document",
            "POST   /api/export": "{path} — write it to a file",
            "POST   /api/self-install": "install this running copy (as the Install button on App Manager's row)",
            "POST   /api/auto-update": "run the auto-update pass now",
            "POST   /api/refresh": "reload the window's list",
            "GET    /api/settings": "API settings",
            "POST   /api/settings": "{api_enabled?, api_port?, api_key?, regenerate_key?}",
        }
    })
}

async fn dispatch(ui: Rc<Ui>, req: &ApiRequest) -> ApiResponse {
    let segs: Vec<String> = req
        .path
        .strip_prefix("/api")
        .unwrap_or(&req.path)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(api::percent_decode)
        .collect();
    let segs: Vec<&str> = segs.iter().map(String::as_str).collect();

    match (req.method.as_str(), segs.as_slice()) {
        ("GET", []) => ApiResponse::ok(index()),
        ("GET", ["docs"]) => match req.string("section") {
            None => ApiResponse::text("text/markdown; charset=utf-8", api::REFERENCE.to_string()),
            Some(name) => match api::reference_section(&name) {
                Some(text) => ApiResponse::text("text/markdown; charset=utf-8", text),
                None => ApiResponse {
                    status: 404,
                    body: json!({ "ok": false, "error": format!("no section matching '{name}'"),
                                  "sections": api::reference_sections() }),
                    raw: None,
                },
            },
        },
        ("GET", ["status"]) => {
            let mut inflight: Vec<String> = ui.inflight.borrow().iter().cloned().collect();
            inflight.sort();
            ApiResponse::ok(json!({
                "ok": true,
                "version": env!("CARGO_PKG_VERSION"),
                "running_from": std::env::current_exe().ok().map(|p| p.display().to_string()),
                "running_appimage": backends::running_appimage().map(|p| p.display().to_string()),
                "self_install_offered": self_install_label(),
                "in_flight": inflight,
                "refreshing": *ui.busy.borrow(),
                "api_port": ui.api.borrow().as_ref().map(|s| s.port()),
                "config_dir": config::config_dir().display().to_string(),
            }))
        }

        ("GET", ["apps"]) => {
            let srcs = config::load_sources().unwrap_or_default();
            let Some(entries) = background(move || srcs.into_iter().map(resolve_entry).collect::<Vec<_>>()).await
            else {
                return ApiResponse::error(500, "the catalog lookup died");
            };
            let apps: Vec<Value> = entries.iter().map(|e| entry_json(&ui, e)).collect();
            ApiResponse::ok(json!({ "ok": true, "apps": apps }))
        }
        ("POST", ["apps"]) => {
            if let Some(src) = req.body.get("source") {
                return match serde_json::from_value::<Source>(src.clone()) {
                    Ok(src) => merge_and_save(&ui, vec![src]),
                    Err(e) => ApiResponse::error(400, &format!("not a valid source: {e}")),
                };
            }
            let Some(input) = req.string("repo").or_else(|| req.string("url")) else {
                return ApiResponse::error(400, "send {repo: \"owner/repo or URL\"} or {source: {...}}");
            };
            match background(move || sources::resolve_repo(&input)).await {
                Some(Ok(src)) => {
                    let added = json!(src);
                    let mut resp = merge_and_save(&ui, vec![src]);
                    if resp.status == 200 {
                        resp.body["source"] = added;
                    }
                    resp
                }
                Some(Err(e)) => ApiResponse::error(422, &format!("couldn't add: {e}")),
                None => ApiResponse::error(500, "the lookup died"),
            }
        }
        ("GET", ["apps", id]) => {
            let Some(src) = find_source(id) else { return no_app(id) };
            match background(move || resolve_entry(src)).await {
                Some(e) => ApiResponse::ok(json!({ "ok": true, "app": entry_json(&ui, &e) })),
                None => ApiResponse::error(500, "the lookup died"),
            }
        }
        ("PATCH", ["apps", id]) => edit_source(&ui, id, req),
        ("DELETE", ["apps", id]) => {
            let Some(src) = find_source(id) else { return no_app(id) };
            match config::remove_source(&src.id) {
                Ok(()) => {
                    refresh(ui.clone());
                    ApiResponse::ok(json!({ "ok": true, "removed": src.id }))
                }
                Err(e) => ApiResponse::error(500, &format!("save failed: {e}")),
            }
        }
        ("POST", ["apps", id, "auto-update"]) => {
            let Some(src) = find_source(id) else { return no_app(id) };
            let Some(on) = req.bool("on") else {
                return ApiResponse::error(400, "send {on: true|false}");
            };
            match config::set_auto_update(&src.id, on) {
                Ok(()) => {
                    refresh(ui.clone());
                    ApiResponse::ok(json!({ "ok": true, "id": src.id, "auto_update": on }))
                }
                Err(e) => ApiResponse::error(500, &format!("save failed: {e}")),
            }
        }
        ("POST", ["apps", id, verb]) => {
            let action = match *verb {
                "install" => Action::Install,
                "update" | "reinstall" => Action::Update,
                "uninstall" | "remove" => Action::Remove,
                "open" => Action::Open,
                other => return ApiResponse::error(404, &format!("no action '{other}' — see GET /api")),
            };
            let Some(src) = find_source(id) else { return no_app(id) };
            let Some(entry) = background(move || resolve_entry(src)).await else {
                return ApiResponse::error(500, "the lookup died");
            };
            if matches!(action, Action::Install | Action::Update) && !entry.installable() {
                return ApiResponse::error(
                    422,
                    &format!(
                        "{}: no downloadable release asset for this machine (GitHub unreachable, \
                         rate-limited, or the release has no matching file)",
                        entry.source.name
                    ),
                );
            }
            let id = entry.source.id.clone();
            match perform(ui.clone(), entry, action).await {
                Ok(message) => ApiResponse::ok(json!({ "ok": true, "id": id, "message": message })),
                Err(e) => ApiResponse::error(if e.busy { 409 } else { 500 }, &e.message),
            }
        }

        ("GET", ["sources"]) => ApiResponse::ok(json!({ "ok": true, "sources": config::load_sources().unwrap_or_default() })),
        ("GET", ["follows"]) => ApiResponse::ok(json!({ "ok": true, "follows": config::load_follows() })),
        ("POST", ["follow"]) => {
            let Some(user) = req.string("user").map(|u| u.trim().to_string()).filter(|u| !u.is_empty()) else {
                return ApiResponse::error(400, "send {user: \"github-username\"}");
            };
            let _ = config::add_follow(&user);
            match background(move || sources::follow_user(&user)).await {
                Some(Ok(list)) => merge_and_save(&ui, list),
                Some(Err(e)) => ApiResponse::error(502, &format!("follow failed: {e}")),
                None => ApiResponse::error(500, "the lookup died"),
            }
        }
        ("POST", ["discover"]) => {
            let follows = config::load_follows();
            match background(move || sources::discover_follows(&follows)).await {
                Some(found) => merge_and_save(&ui, found),
                None => ApiResponse::error(500, "the lookup died"),
            }
        }
        ("POST", ["import-official"]) => match background(sources::import_official).await {
            Some(Ok(list)) => merge_and_save(&ui, list),
            Some(Err(e)) => ApiResponse::error(502, &format!("import failed: {e}")),
            None => ApiResponse::error(500, "the lookup died"),
        },
        ("POST", ["import"]) => {
            let text = match (req.body.get("config"), req.string("path")) {
                (Some(Value::String(t)), _) => Ok(t.clone()),
                (Some(doc), _) => Ok(doc.to_string()),
                (None, Some(path)) => std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}")),
                (None, None) => Err("send {config: <document>} or {path: \"/file.json\"}".to_string()),
            };
            match text.map(|t| sources::parse_config(&t).map_err(|e| format!("bad config: {e}"))) {
                Ok(Ok(list)) => merge_and_save(&ui, list),
                Ok(Err(e)) | Err(e) => ApiResponse::error(400, &e),
            }
        }
        ("GET", ["export"]) => ApiResponse::ok(config::config_doc(&config::load_sources().unwrap_or_default())),
        ("POST", ["export"]) => {
            let Some(path) = req.string("path") else {
                return ApiResponse::error(400, "send {path: \"/file.json\"} — or GET /api/export for the document");
            };
            let srcs = config::load_sources().unwrap_or_default();
            match config::export_config(&srcs, std::path::Path::new(&path)) {
                Ok(()) => ApiResponse::ok(json!({ "ok": true, "path": path, "sources": srcs.len() })),
                Err(e) => ApiResponse::error(500, &format!("export failed: {e}")),
            }
        }
        ("POST", ["self-install"]) => match perform_self_install(ui.clone()).await {
            Ok(path) => ApiResponse::ok(json!({ "ok": true, "installed_to": path })),
            Err(e) => ApiResponse::error(500, &e),
        },
        ("POST", ["auto-update"]) => {
            let srcs = config::load_sources().unwrap_or_default();
            match background(move || catalog::auto_update(&srcs)).await {
                Some(r) => {
                    refresh(ui.clone());
                    let failed: Vec<Value> = r.failed.iter().map(|(n, e)| json!({ "name": n, "error": e })).collect();
                    ApiResponse::ok(json!({ "ok": r.failed.is_empty(), "updated": r.updated, "failed": failed }))
                }
                None => ApiResponse::error(500, "the auto-update pass died"),
            }
        }
        ("POST", ["refresh"]) => {
            refresh(ui.clone());
            ApiResponse::ok(json!({ "ok": true }))
        }

        ("GET", ["settings"]) => {
            let s = config::load_settings();
            ApiResponse::ok(json!({ "ok": true, "api_enabled": s.api_enabled, "api_port": s.api_port,
                                    "listening_on": ui.api.borrow().as_ref().map(|s| s.port()) }))
        }
        ("POST", ["settings"]) => api_settings(&ui, req),

        (_, []) | (_, ["docs" | "status" | "apps" | "sources" | "follows" | "follow" | "discover"
            | "import-official" | "import" | "export" | "self-install" | "auto-update" | "refresh"
            | "settings", ..]) => ApiResponse::error(405, &format!("{} is not supported on {}", req.method, req.path)),
        _ => ApiResponse::error(404, &format!("no route {} {} — GET /api lists them", req.method, req.path)),
    }
}

/// PATCH /api/apps/{id}: the Edit… dialog, field by field. The id stays put —
/// it names the install on disk.
fn edit_source(ui: &Rc<Ui>, needle: &str, req: &ApiRequest) -> ApiResponse {
    let Some(found) = find_source(needle) else { return no_app(needle) };
    let mut srcs = config::load_sources().unwrap_or_default();
    let Some(src) = srcs.iter_mut().find(|s| s.id == found.id) else { return no_app(needle) };

    if let Some(v) = req.string("name") {
        src.name = v;
    }
    if req.body.get("description").is_some() {
        src.description = req.string("description").filter(|d| !d.trim().is_empty());
    }
    if let Some(k) = req.body.get("kind") {
        match serde_json::from_value::<Kind>(k.clone()) {
            Ok(k) => src.kind = k,
            Err(_) => return ApiResponse::error(400, "kind must be bin, appimage, deb or tar"),
        }
    }
    if req.body.get("package").is_some() {
        src.package = req.string("package").filter(|p| !p.trim().is_empty());
    }
    if req.body.get("install_path").is_some() {
        src.install_path = req.string("install_path").filter(|p| !p.trim().is_empty());
    }
    if let Some(repo) = req.string("repo") {
        src.origin = Origin::Github { repo: normalize_repo(&repo) };
    } else if let Some(url) = req.string("url") {
        src.origin = Origin::Url { url };
    } else if let Some(path) = req.string("path") {
        src.origin = Origin::Local { path };
    }
    if let Some(b) = req.bool("auto_update") {
        src.auto_update = b;
    }
    if let Some(b) = req.bool("cli") {
        src.cli = b;
    }
    let edited = json!(src);
    match config::save_sources(&srcs) {
        Ok(()) => {
            refresh(ui.clone());
            ApiResponse::ok(json!({ "ok": true, "source": edited }))
        }
        Err(e) => ApiResponse::error(500, &format!("save failed: {e}")),
    }
}

/// POST /api/settings. The reply to this request still goes out on the old
/// connection, so changing the key or port here is safe — the *next* call has
/// to use the new one.
fn api_settings(ui: &Rc<Ui>, req: &ApiRequest) -> ApiResponse {
    let mut s = config::load_settings();
    let mut key_changed = false;
    if req.bool("regenerate_key") == Some(true) {
        match api::regenerate_key() {
            Some(k) => *ui.api_key.borrow_mut() = k,
            None => return ApiResponse::error(500, "could not write the key file"),
        }
        key_changed = true;
    } else if let Some(k) = req.string("api_key") {
        match api::set_key(&k) {
            Ok(k) => *ui.api_key.borrow_mut() = k,
            Err(e) => return ApiResponse::error(400, &e),
        }
        key_changed = true;
    }
    if let Some(p) = req.u64("api_port") {
        match u16::try_from(p) {
            Ok(p) if p >= 1024 => s.api_port = p,
            _ => return ApiResponse::error(400, "api_port must be 1024–65535"),
        }
    }
    if let Some(on) = req.bool("api_enabled") {
        s.api_enabled = on;
    }

    let running = ui.api.borrow().as_ref().map(|s| s.port());
    if s.api_enabled && (running != Some(s.api_port) || key_changed) {
        if let Err(e) = rebind_api(ui, s.api_port) {
            // The key is already on disk, so the listener that stayed up has to
            // take it too, or the old key would keep working.
            if let (true, Some(p)) = (key_changed, running) {
                let _ = rebind_api(ui, p);
            }
            let stays = running.map_or("off".to_string(), |p| format!("on port {p}"));
            return ApiResponse::error(409, &format!("could not listen on port {}: {e} — the API stays {stays}", s.api_port));
        }
    } else if !s.api_enabled {
        *ui.api.borrow_mut() = None;
    }
    if let Err(e) = config::save_settings(&s) {
        return ApiResponse::error(500, &format!("save failed: {e}"));
    }
    let mut body = json!({ "ok": true, "api_enabled": s.api_enabled, "api_port": s.api_port });
    if key_changed {
        body["api_key"] = json!(ui.api_key.borrow().clone());
    }
    ApiResponse::ok(body)
}
