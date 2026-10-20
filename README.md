# Game Anatomy

Compute RetroAchievements-compatible MD5 hashes from disc/games images using
RetroAchievements-compatible MD5 hashes.

## CLI

Requires Rust with edition 2024 support.

```sh
cargo run --release -- "game.iso"
```

Prints the lowercase MD5. Errors go to stderr with a nonzero exit status.
Accepts ISO, BIN, CUE, MDF, and PSP PBP files. For MDS/MDF pairs, pass the MDF.

## Python

Build the Linux extension with Rust and Make, then add its output to `PYTHONPATH`:

```sh
make -C bindings/python
export PYTHONPATH="$PWD/bindings/python/out${PYTHONPATH:+:$PYTHONPATH}"
```

The default build targets x86-64 Linux and CPython 3.10+.

```python
from game_anatomy import generate_hash

info = generate_hash("game.iso")  # also accepts pathlib.Path
# {
#     "platform": "ps2",
#     "serial": "SLUS_208.31",
#     "identity_md5": "5a3355677a9bd77143904deb8b8ee893",
#     "retroachievements_platform_id": 21,
# }
```

Serial text is returned without normalization, or `None` when unavailable.
Hashing failures raise `RuntimeError`. No RetroAchievements account or game
lookup is performed; the platform ID is a console ID, not a game ID.

## Rust

`game_anatomy::generate_hash(&Path)` returns the hash;
`game_anatomy::game_info(&Path)` returns the hash and metadata.
