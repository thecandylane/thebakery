# Session environment (the room)

**Status:** contract for slice 01. Living, short.

A **socket** has one **room**. The room stays up. Commands, your terminal, and (later) language servers walk *into* it. The room is not a side effect of `run`.

```
Socket
└── SessionEnvironment     ← always (thin). Lives across commands.
    ├── thin runtime (HOME/tmp, Seatbelt, /bin+/usr/bin)
    ├── optional overlay (mise / Bakery pin / named host bind)
    ├── human: host PTY (default, trusted) and/or room PTY
    ├── agent: ACP + exec attach here, never host env
    └── later: LSP, named services
```

The environment **specification belongs to Bakery** (socket config on the node). A repo `mise.toml` is one **import adapter**. Exporting Bakery’s pin back to mise is optional.

## Spec (optional overlay)

A socket **always** has a thin room. A pack file is not admission.

- **Thin (default, 07b):** no `mise.toml` required. Boxed HOME, worktree, `/bin`+`/usr/bin`. ACP and room terminal attach here. Linux kernel, GreenLeaf, any git repo.
- **Overlay (optional, later):** `mise.toml`, `.python-version`, `go.mod`, a Bakery-stored pin, or a **named** host tool. Cache by digest globally; never share socket HOME.

The node reports layers to the client. Do not default every project to Rust.

## Provision vs attach

**Provision** (node, trusted): may use host `rustup`/`mise`, network, APFS clone. Writes a cache dir keyed by digest. Creates `ready` when tools are in the room.

**Attach** (untrusted relative to the host): `env -i`, PATH only the room + `/bin` + `/usr/bin`, HOME inside the room, no host `HOME`, no inherited `*_API_KEY` / `BAKERY_*` / `HOST_SECRET`.

Second provision with the same digest is a **cache hit** (no toolchain copy/install).

## Isolation (this Mac, slice 01)

No Docker / Apple `container` installed today. Do not block the slice on them.

Proven here (2026-08-26):

- APFS `cp -cR` of a rustup toolchain into the room — `rustc` works
- `env -i` — host env secrets do not appear; PATH has no Homebrew / `~/.cargo`
- `/usr/bin/sandbox-exec` — **write** outside the room denied; **read** of a secrets directory denied if listed in the profile
- `/usr/bin/script` — PTY attach; same `rustc --version` as plain exec

`enforcement_tier` for this backend: `seatbelt-prefix`. Honest: not a VM. Apple container is a later adapter with a higher tier. Fail closed if a session *requires* a higher tier than we have.

Cache dir lives under the node cache (e.g. `~/.bakery/cache/envs/<digest>/`), **not** mixed into secrets. Seatbelt must deny read of `~/.bakery/secrets.json` and `heart.db*`.

## Proof (slice 01 is done only if)

Using The Bakery repo as the workspace:

1. Provision the room from the pinned spec.
2. Agent exec: `rustc --version`
3. Human PTY attach: `rustc --version`
4. The two version strings match.
5. Provision again → cache hit.
6. A host env var like `HOST_SECRET` is empty inside the room.
7. PATH inside the room does not contain `/opt/homebrew` or the host `~/.cargo`.
8. Reading a planted secrets file outside the room is denied.

No Flutter, no Mini, no studio terminal widget, no LSP in this slice. HTTP to drive provision/exec/pty from the node is allowed so we can dogfood without a UI. The studio dock is slice 02.

## Persistent ACP seats (current Mac)

ACP children use the same contained project world but keep duplex protocol
stdio and a private HOME per `(stream, seat)`. Trusted Slice/Living streams may
write the worktree; changing the stream to Burn revokes cached trusted models,
kills/reaps their process generations, and rechecks lifetime before any later
trusted spawn. Burn ACP remains boxed and worktree-read-only.

Optional CLI config is copied from exactly one operator-named directory to an
explicit relative path beneath the private seat HOME:

```json
"acp_config": {
  "source": "/absolute/path/to/one-plug-config-directory",
  "home_relative_destination": ".config/that-plug"
}
```

Both paths are checked before spawn. `/`, host HOME, `~/.bakery`, missing or
non-directory sources, absolute destinations, traversal, and destination
symlink escapes fail as Setup. No host HOME or Keychain is mounted.
