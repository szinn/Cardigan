# Cardigan

A bi-directional CardDAV sync between Apple Contacts (iCloud) and Fastmail.

Cardigan pairs the contacts in your iCloud and Fastmail address books, then keeps them in sync:
creates, edits, deletes and photos made on either side are copied to the other. It keeps its sync
state in a small SQLite database, so it knows what changed since the last cycle.

## Configuration

Cardigan is configured entirely through environment variables. Blank values count as unset.

| Variable                      | Required | Default                        | Description                                                                                                                    |
| ----------------------------- | -------- | ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------ |
| `CARDIGAN_ICLOUD_USERNAME`    | yes      |                                | Your Apple ID.                                                                                                                 |
| `CARDIGAN_ICLOUD_PASSWORD`    | yes      |                                | An [app-specific password](https://support.apple.com/en-us/102654) for your Apple ID.                                          |
| `CARDIGAN_FASTMAIL_USERNAME`  | yes      |                                | Your Fastmail login.                                                                                                           |
| `CARDIGAN_FASTMAIL_PASSWORD`  | yes      |                                | A Fastmail [app password](https://www.fastmail.help/hc/en-us/articles/360058752854) with CardDAV access.                       |
| `CARDIGAN_DATABASE_PATH`      | yes      |                                | An existing directory for the state database. Cardigan creates `cardigan.db` inside it. The path may not contain `?` or `%`.   |
| `CARDIGAN_ICLOUD_URL`         | no       | `https://contacts.icloud.com`  | iCloud CardDAV discovery URL.                                                                                                  |
| `CARDIGAN_FASTMAIL_URL`       | no       | `https://carddav.fastmail.com` | Fastmail CardDAV discovery URL.                                                                                                |
| `CARDIGAN_POLL_INTERVAL_SECS` | no       | `120`                          | Seconds between sync cycles in daemon mode. Minimum `60`.                                                                      |
| `CARDIGAN_CONFLICT_WINNER`    | no       | `icloud`                       | Which side wins when the same contact was edited on both sides since the last sync: `icloud` or `fastmail` (case-insensitive). |
| `RUST_LOG`                    | no       | `info`                         | Log filter for `sync` and `dry-run` (e.g. `debug`, `cardigan=debug,info`). Logs go to stdout. `dump` does not log.             |

If any required variable is missing, Cardigan exits and names every missing one.

> [!IMPORTANT]
> An iCloud app-specific password grants access to **all** DAV services on your Apple ID
> (contacts, calendars and reminders), not just contacts. Store it like your main password.

## Commands

```text
cardigan <COMMAND>

Commands:
  sync      Sync iCloud and Fastmail contacts continuously
  dry-run   Print the sync plan without changing either server or the sync state
  dump      Print every card of one side's address book as JSON, without syncing
  help      Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

### `cardigan sync`

Runs a sync cycle immediately, then every `CARDIGAN_POLL_INTERVAL_SECS`, until it receives SIGINT
or SIGTERM. A cycle in progress finishes before Cardigan exits. A failed cycle is logged and
retried at the next interval, or after the server's `Retry-After` if that is later.

| Option    | Description                                                                                                         |
| --------- | ------------------------------------------------------------------------------------------------------------------- |
| `--once`  | Run a single sync cycle and exit. Exits non-zero if the cycle fails or the mass-deletion guard blocks it.           |
| `--reset` | Drop the sync state and re-baseline before syncing: every contact is paired again from scratch. Nothing is deleted. |

### `cardigan dry-run`

Lists and pairs both address books and prints the plan the next `sync` cycle would carry out.
It writes nothing to either server or to the sync state. It succeeds even when the
mass-deletion guard would block the sync, and says so at the top of the report.

| Option    | Description                                                                       |
| --------- | --------------------------------------------------------------------------------- |
| `--reset` | Preview a re-baseline: plan as if the sync state were empty, without dropping it. |

### `cardigan dump <icloud|fastmail>`

Prints every card in one side's address book as JSON on stdout, for inspection or backup. It
needs the credentials but never opens the state database and never syncs.

```sh
cardigan dump icloud > icloud-contacts.json
```

## First run

1. Back up both address books, for example with `cardigan dump icloud` and `cardigan dump fastmail`.
2. Run `cardigan dry-run` and read the plan. On the first run Cardigan pairs contacts that exist on
   both sides and copies the ones that exist on only one side. Contacts it can't pair with
   confidence are listed as skipped and never guessed.
3. When the plan looks right, run `cardigan sync`.

To start over from scratch later, preview with `cardigan dry-run --reset`, then run
`cardigan sync --reset`.

## Safety

- **Mass-deletion guard:** if a cycle would delete more than 20% of the synced contacts on either
  side (and more than 10), Cardigan writes nothing and logs why. `sync --once` exits non-zero.
  Run `cardigan dry-run` to see the plan.
- **Held deletes:** when a contact deleted on one side looks like the same contact as a different
  pair deleted on the other side, both deletes are held. Edit the copy you want to keep, or delete
  every remaining copy.
- **One syncing client:** don't let any other device or app edit both accounts. A device showing
  iCloud and Fastmail together in Contacts merges cards by name, so one delete there can remove
  cards from two different pairs, and Cardigan then copies both deletes.

## Docker

The release image is `ghcr.io/szinn/cardigan`. Its entrypoint is `cardigan sync`, so any arguments
are passed to `sync`. The container runs as uid/gid `1234`, so mount a directory for the state
database that this user can write:

```sh
mkdir -p ./data && sudo chown 1234:1234 ./data
docker run -d --name cardigan \
  -e CARDIGAN_ICLOUD_USERNAME=… -e CARDIGAN_ICLOUD_PASSWORD=… \
  -e CARDIGAN_FASTMAIL_USERNAME=… -e CARDIGAN_FASTMAIL_PASSWORD=… \
  -e CARDIGAN_DATABASE_PATH=/data \
  -v "$PWD/data:/data" \
  ghcr.io/szinn/cardigan:latest
```

To run `dry-run` or `dump` with the image, override the entrypoint:
`docker run --rm --entrypoint /app/cardigan … ghcr.io/szinn/cardigan:latest dry-run`.

## Building

```sh
mise run build   # build
mise run test    # run the tests
```
