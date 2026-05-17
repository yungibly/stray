PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
TARGET ?= release
SUDO ?= sudo

BIN = stray
CMD = stray

all:
	cargo build --$(TARGET)

install: all
	$(SUDO) install -Dm755 target/$(TARGET)/$(BIN) $(DESTDIR)$(BINDIR)/$(CMD)

uninstall:
	$(SUDO) rm -f $(DESTDIR)$(BINDIR)/$(CMD)

clean:
	cargo clean

.PHONY: all install uninstall clean
