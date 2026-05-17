use std::collections::HashMap;
use std::error::Error;
use std::io::ErrorKind;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, sleep};
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Structure, Value};
use zbus::{Connection, Proxy, connection, interface};

const MENU_CMD: &str = "wmenu";
const MENU_ARGS: &[&str] = &["-i"];
const TEMPORARY_WATCHER_SETTLE_MS: u64 = 700;

const WATCHER_SERVICE: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const WATCHER_IFACE: &str = "org.kde.StatusNotifierWatcher";
const ITEM_IFACE: &str = "org.kde.StatusNotifierItem";
const DBUS_MENU_IFACE: &str = "com.canonical.dbusmenu";

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type MenuNode = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

#[derive(Clone)]
struct TrayItem {
    service: String,
    path: String,
    label: String,
}

struct MenuAction {
    id: i32,
    label: String,
}

#[derive(Clone, Default)]
struct TemporaryWatcher {
    items: Arc<Mutex<Vec<String>>>,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl TemporaryWatcher {
    fn register_status_notifier_item(&self, service: &str, #[zbus(header)] header: Header<'_>) {
        let Some(item) = normalize_registered_item(
            service,
            header.sender().map(|sender| sender.as_str().to_string()),
        ) else {
            return;
        };

        let mut items = self.items.lock().expect("watcher items mutex poisoned");
        if !items.iter().any(|registered| registered == &item) {
            items.push(item);
        }
    }

    fn register_status_notifier_host(&self, _service: &str) {}

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items
            .lock()
            .expect("watcher items mutex poisoned")
            .clone()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut connection = Connection::session().await?;
    let mut tried_temporary_watcher = false;
    let items = match tray_items(&connection).await {
        Ok(items) => items,
        Err(error) if missing_watcher(&error) => {
            tried_temporary_watcher = true;
            eprintln!(
                "stray: no StatusNotifierWatcher found; listening for tray items for {TEMPORARY_WATCHER_SETTLE_MS}ms"
            );
            let (watcher_connection, items) = temporary_tray_items().await?;
            connection = watcher_connection;
            items
        }
        Err(error) => return Err(error.into()),
    };

    if items.is_empty() {
        if tried_temporary_watcher {
            eprintln!(
                "stray: no StatusNotifier items registered; run a tray host or rerun after apps notice the watcher"
            );
        } else {
            eprintln!("stray: no StatusNotifier items found");
        }
        return Ok(());
    }

    let item_lines = numbered_lines(items.iter().map(|item| item.label.as_str()));
    let Some(selection) = run_menu(&item_lines).await? else {
        return Ok(());
    };
    let Some(item_index) = selected_index(&selection, items.len()) else {
        return Ok(());
    };

    let item = &items[item_index];
    let actions = menu_actions(&connection, item).await?;

    if actions.is_empty() {
        activate_item(&connection, item).await?;
        return Ok(());
    }

    let action_lines = numbered_lines(actions.iter().map(|action| action.label.as_str()));
    let Some(selection) = run_menu(&action_lines).await? else {
        return Ok(());
    };
    let Some(action_index) = selected_index(&selection, actions.len()) else {
        return Ok(());
    };

    click_menu_action(&connection, item, actions[action_index].id).await
}

async fn tray_items(connection: &Connection) -> zbus::Result<Vec<TrayItem>> {
    let watcher = Proxy::new(connection, WATCHER_SERVICE, WATCHER_PATH, WATCHER_IFACE).await?;
    let registered: Vec<String> = watcher
        .get_property("RegisteredStatusNotifierItems")
        .await?;
    Ok(items_from_addresses(connection, registered).await)
}

async fn temporary_tray_items() -> Result<(Connection, Vec<TrayItem>)> {
    let watcher = TemporaryWatcher::default();
    let registered = watcher.items.clone();
    let connection = connection::Builder::session()?
        .name(WATCHER_SERVICE)?
        .serve_at(WATCHER_PATH, watcher)?
        .build()
        .await?;

    sleep(Duration::from_millis(TEMPORARY_WATCHER_SETTLE_MS)).await;
    let registered = registered
        .lock()
        .expect("watcher items mutex poisoned")
        .clone();
    let items = items_from_addresses(&connection, registered).await;

    Ok((connection, items))
}

async fn items_from_addresses(connection: &Connection, registered: Vec<String>) -> Vec<TrayItem> {
    let mut items = Vec::new();

    for address in registered {
        let Some((service, path)) = split_item_address(&address) else {
            continue;
        };
        let item = TrayItem {
            label: item_label(connection, &service, &path).await,
            service,
            path,
        };
        items.push(item);
    }

    items
}

fn missing_watcher(error: &zbus::Error) -> bool {
    matches!(
        error,
        zbus::Error::MethodError(name, _, _)
            if name.as_ref().as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
    )
}

fn normalize_registered_item(service: &str, sender: Option<String>) -> Option<String> {
    let service = service.trim();
    if service.is_empty() {
        return None;
    }

    if service.starts_with('/') {
        Some(format!("{}{service}", sender?))
    } else {
        Some(service.to_string())
    }
}

fn split_item_address(address: &str) -> Option<(String, String)> {
    if let Some((service, path)) = address.split_once('/') {
        if service.is_empty() {
            return None;
        }
        return Some((service.to_string(), format!("/{path}")));
    }

    if address.is_empty() {
        None
    } else {
        Some((address.to_string(), "/StatusNotifierItem".to_string()))
    }
}

async fn item_label(connection: &Connection, service: &str, path: &str) -> String {
    let fallback = service.to_string();
    let Ok(proxy) = Proxy::new(connection, service, path, ITEM_IFACE).await else {
        return fallback;
    };

    for property in ["Title", "Id"] {
        if let Ok(value) = proxy.get_property::<String>(property).await {
            let value = value.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }

    fallback
}

async fn menu_actions(connection: &Connection, item: &TrayItem) -> Result<Vec<MenuAction>> {
    let item_proxy = Proxy::new(
        connection,
        item.service.as_str(),
        item.path.as_str(),
        ITEM_IFACE,
    )
    .await?;
    let menu_path: OwnedObjectPath = item_proxy.get_property("Menu").await?;

    if menu_path.as_str() == "/" {
        return Ok(Vec::new());
    }

    let menu = Proxy::new(
        connection,
        item.service.as_str(),
        menu_path.as_str(),
        DBUS_MENU_IFACE,
    )
    .await?;
    let (_revision, root): (u32, MenuNode) = menu
        .call("GetLayout", &(0_i32, -1_i32, Vec::<&str>::new()))
        .await?;
    let mut actions = Vec::new();

    collect_actions(root, "", &mut actions)?;
    Ok(actions)
}

fn collect_actions(node: MenuNode, parent: &str, actions: &mut Vec<MenuAction>) -> Result<()> {
    let (id, props, children) = node;

    if prop_bool(&props, "visible").unwrap_or(true)
        && prop_string(&props, "type").as_deref() != Some("separator")
    {
        let label = prop_string(&props, "label")
            .map(clean_label)
            .unwrap_or_default();
        let path = if parent.is_empty() {
            label.clone()
        } else if label.is_empty() {
            parent.to_string()
        } else {
            format!("{parent} > {label}")
        };

        if children.is_empty() {
            if id != 0 && !path.is_empty() && prop_bool(&props, "enabled").unwrap_or(true) {
                actions.push(MenuAction {
                    id,
                    label: with_toggle(&props, path),
                });
            }
        } else {
            for child in children {
                collect_actions(menu_node(child)?, &path, actions)?;
            }
        }
    }

    Ok(())
}

fn menu_node(value: OwnedValue) -> Result<MenuNode> {
    let structure = Structure::try_from(value)?;
    let node = MenuNode::try_from(structure)?;
    Ok(node)
}

fn prop_string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|value| String::try_from(value.clone()).ok())
}

fn prop_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    props
        .get(key)
        .and_then(|value| bool::try_from(value.clone()).ok())
}

fn prop_i32(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i32> {
    props
        .get(key)
        .and_then(|value| i32::try_from(value.clone()).ok())
}

fn clean_label(label: String) -> String {
    let mut cleaned = String::new();
    let mut chars = label.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '_' {
            if chars.peek() == Some(&'_') {
                cleaned.push('_');
                chars.next();
            }
        } else {
            cleaned.push(ch);
        }
    }

    cleaned
}

fn with_toggle(props: &HashMap<String, OwnedValue>, label: String) -> String {
    if prop_string(props, "toggle-type").is_none() {
        return label;
    }

    match prop_i32(props, "toggle-state") {
        Some(1) => format!("[x] {label}"),
        Some(0) => format!("[ ] {label}"),
        _ => format!("[-] {label}"),
    }
}

async fn click_menu_action(connection: &Connection, item: &TrayItem, id: i32) -> Result<()> {
    let item_proxy = Proxy::new(
        connection,
        item.service.as_str(),
        item.path.as_str(),
        ITEM_IFACE,
    )
    .await?;
    let menu_path: OwnedObjectPath = item_proxy.get_property("Menu").await?;
    let menu = Proxy::new(
        connection,
        item.service.as_str(),
        menu_path.as_str(),
        DBUS_MENU_IFACE,
    )
    .await?;
    let timestamp = monotonicish_timestamp();

    menu.call_noreply("Event", &(id, "clicked", Value::from(0_i32), timestamp))
        .await?;
    Ok(())
}

async fn activate_item(connection: &Connection, item: &TrayItem) -> Result<()> {
    let proxy = Proxy::new(
        connection,
        item.service.as_str(),
        item.path.as_str(),
        ITEM_IFACE,
    )
    .await?;
    proxy.call_noreply("Activate", &(0_i32, 0_i32)).await?;
    Ok(())
}

fn monotonicish_timestamp() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u32)
        .unwrap_or(0)
}

fn numbered_lines<'a>(labels: impl Iterator<Item = &'a str>) -> Vec<String> {
    labels
        .enumerate()
        .map(|(index, label)| format!("{}\t{}", index + 1, label))
        .collect()
}

fn selected_index(selection: &str, len: usize) -> Option<usize> {
    let (number, _) = selection.split_once('\t')?;
    let index = number.trim().parse::<usize>().ok()?.checked_sub(1)?;

    (index < len).then_some(index)
}

async fn run_menu(lines: &[String]) -> Result<Option<String>> {
    let mut command = Command::new(MENU_CMD);
    command.args(MENU_ARGS);
    command.stdin(Stdio::piped()).stdout(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(format!("stray: could not run `{MENU_CMD}`").into());
        }
        Err(error) => return Err(error.into()),
    };

    let mut stdin = child.stdin.take().expect("stdin is piped");
    for line in lines {
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    }
    drop(stdin);

    let output = child.wait_with_output().await?;
    if !output.status.success() {
        return Ok(None);
    }

    let selection = String::from_utf8_lossy(&output.stdout)
        .trim_end_matches('\n')
        .to_string();

    if selection.is_empty() {
        Ok(None)
    } else {
        Ok(Some(selection))
    }
}
