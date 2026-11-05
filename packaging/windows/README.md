# Windows Setup preview

`ramwarden.iss` defines a conventional offline EXE wizard (Welcome, license,
folder, shortcuts, progress, finish) with the existing pixel logo and a standard
Apps & Features uninstaller. The Rust tray executable is also the app launcher.
The payload is an explicit six-binary list; user configuration, credentials and
account tokens are never included or deleted by this setup definition.

Compile with an installed Inno Setup compiler using
`scripts/build-windows-setup.ps1`. Supply a payload directory, semantic version,
compiler path and output directory. This source has not yet been compiled or
accepted on a clean machine. Do not distribute it as a release.

Outstanding: signed payload and installer; install/startup/task/service lifecycle;
original-version upgrade and rollback; silent install/uninstall; branded wizard
sidebar artwork; optional safe downloaded-installer cleanup; hosted account AI;
clean-machine sign-in and sensor setup. Existing developer scheduled tasks must
retain their configuration and private routing wrapper during an upgrade.
See `../WINDOWS_STORE.md` for the complete release gates.

Warp is pinned as the `third_party/warp` submodule for source reuse. Its original
licenses remain in that checkout. No Warp runtime code has been copied into this
installer or the current egui UI. Integrate components only where they fit the
existing Rust architecture, retaining the applicable notices and licenses.
