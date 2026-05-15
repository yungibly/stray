Simple on-demand tray program. Uses your existing menu (wmenu by default, but could use fuzzel, wofi, etc) to display tray programs and select their options.
This way, if you rarely interact with programs in the tray, you can still do so when you want to, but without having a constant listener or adding complexity to your setup.

## Usage

To install, just run `make install` in the root with the Makefile. If you want it to go into .local/bin instead of /usr/, run `make install PREFIX=$HOME/.local SUDO=`

To use it, just run `stray`.

To configure the menu command used, edit these two lines in main.rs for the actual command and args respectively.

```rust
const MENU_CMD: &str = "wmenu";
const MENU_ARGS: &[&str] = &["-i"];
```

You can also change the amount of time in ms that the program waits before displaying what it fetched:

```rust
const TEMPORARY_WATCHER_SETTLE_MS: u64 = 700;
```

## How it works

Programs that want to display in the tray try to register with a watcher, but if there isn't one, they just watch and wait for one to appear. This program appears, posing as "org.kde.StatusNotifierWatcher", which programs know is a valid watcher, and then they submit their register again. Typically, 700ms is more than enough for all of this to take place.

