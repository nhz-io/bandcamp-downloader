# bcdl

This tool downloads the music that you bought on Bandcamp. It keeps a record of the
music with the files. Subsequent runs read this record and know which files they have.

## Install

```sh
brew install nhz-io/tap/bcdl
```

Or get a binary from the [releases](https://github.com/nhz-io/bandcamp-downloader/releases)
page. There are binaries for macOS (Apple silicon and Intel), Linux, and Windows.

```sh
tar -xzf bcdl-v2.0.0-aarch64-apple-darwin.tar.gz
xattr -d com.apple.quarantine bcdl     # macOS only, read the note below
install -m755 bcdl /usr/local/bin/
```

On macOS, use the `xattr` command. The binary has no signature. If a browser downloaded
the archive, macOS puts a quarantine attribute on it. `tar` gives this attribute to the
file that it extracts, and `install` keeps it. macOS then stops the program with signal
9 and shows no message.

Or build it:

```sh
cargo install --path .
```

## Use

```sh
bcdl                                # download all music, log in from your browser
bcdl -o ~/Music/bandcamp            # use a different directory
bcdl -f mp3-320                     # use a different format
bcdl list ott                       # show the items that match "ott"
bcdl list --since 3m                # show the items that you bought in 3 months
bcdl download --artist ott          # download the music of one artist only
bcdl verify                         # compare the files with the record, offline
bcdl diff                           # show the changes between the kept versions
```

If you give no command, the tool downloads the music.

## Options

| Option | Function |
|---|---|
| `-f, --format` | `flac` (default), `mp3-320`, `mp3-v0`, `aac-hi`, `aiff-lossless`, `alac`, `vorbis`, `wav` |
| `-o, --output` | The directory for the music and the record |
| `--artist`, `--album` | Select the items that contain this text in the name |
| `--since`, `--until` | Select the items by date: `7d`, `2w`, `3m`, `1y`, or `2024-01-03` |
| `--recheck` | Download the items that you have, to find the changes |
| `--browser`, `--profile` | Select where to read the login from |
| `--delay` | The milliseconds between requests (150 is the default, `0` stops the delay) |

In `--since 3m`, the letter `m` is months. This option selects items by the purchase
date, thus a unit that is less than one day has no function.

Bandcamp does not offer all albums in all formats. The tool reports an album that it
cannot get in the selected format, then continues.

The exit status is `1` if a subsequent run can correct the problem. The exit status is
`0` if no run can correct the problem.

## How the tool logs in

You do not have to copy a token. The tool reads the `identity` cookie from a browser
that is logged in. It reads Firefox first. Then it reads Chrome, Chromium, Edge, Brave,
and Vivaldi.

The tool examines all profiles and uses the login that you used last. If more than one
profile has a login, the tool prints its selection. Use `--browser` or `--profile` to
select a different one.

If the computer has no browser, use `--identity` or set `BANDCAMP_IDENTITY`.

## Your files

The tool does not replace or delete a file that it downloaded before.

If Bandcamp changes an album, the tool moves your copy to `.versions/` and keeps it.
Then it reports which tracks are different. If a download stops, the tool continues it
from that point.

The tool writes its record to `.bandcamp-manifest.json`, with the music. The command
`bcdl verify` compares the files with this record and does not use the network.

## Licence

MIT
