# Security

## Report a vulnerability

Report it privately at
<https://github.com/BurgessTG/eddie/security/advisories/new> (the
repository's Security tab, "Report a vulnerability"). Please do not open a
public issue for it.

Only the latest release and `master` get fixes.

## What Eddie protects

- **Scripts are sandboxed.** `eddie run` and the JS console run QuickJS
  with no file, network or operating-system access, a memory limit and a
  time limit. A script can only queue edits to the open project. A script
  that reads or writes anything else is a vulnerability.
- **The live socket is yours alone.** A running editor listens on a Unix
  socket with mode 0600 in a folder with mode 0700, under
  `$XDG_RUNTIME_DIR/eddie`. Any program running as your user can drive the
  editor through it; that is what it is for. Another user reaching it is a
  vulnerability.
- **Files are not overwritten by accident.** Export, `sheet` and `strip`
  refuse an output path that is the project or one of its media files.
- **Keys stay local.** A cloud transcription key is stored in the settings
  file, which is created private on Unix, and is sent only to the provider
  you configured. Local transcription needs no key and sends nothing.

## What it does not

- Media is decoded by FFmpeg. Keep FFmpeg up to date, and treat a file from
  an untrusted source as you would in any other player.
- A project file names media by path, and opening it imports those files.
  Open projects you trust.
