# qtfblight

QTFB server for reMarkable tablets using `libblight`, with AppLoad QTFB feature
parity.

## Building

Use the matching reMarkable SDK:

| Devices | Rust target |
| --- | --- |
| reMarkable 1 and 2 | `armv7-unknown-linux-gnueabihf` |
| Paper Pro, Paper Pro Move, and newer 64-bit models | `aarch64-unknown-linux-gnu` |

## Run

Assuming you installed qtfblight using Vellum, run:
```sh
qtfblight /path/to/qtfb-client --client-argument
```

## Oxide

Template: [`examples/qtfblight.oxide`](examples/qtfblight.oxide).

Convert an AppLoad app directory or `external.manifest.json`:

```sh
appload-to-oxide /home/root/xovi/exthome/appload/example-client \
  --output /home/root/.local/share/applications/example-client.oxide
```

`"qtfb": true` uses `/home/root/.vellum/bin/qtfblight`. Shim manifests and KOReader are
skipped. `icon` is included only when `icon.png` exists.

## Chroot clients

Bind both the QTFB socket and `/dev/shm`:

```sh
mount --bind /dev/shm /path/to/chroot/dev/shm
mount --bind /tmp/qtfb.sock /path/to/chroot/tmp/qtfb.sock
```

## License

Licensed under the [GNU General Public License v3.0 only](LICENSE).
