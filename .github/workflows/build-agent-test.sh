on: [push]
name: Build agent on multiple rust versions
jobs:
  build_1:
    name: Build-nightly
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - name: Install toolchain
        uses: actions-rs/toolchain@v1
        with:
            toolchain: nightly
            target: x86_64-unknown-linux-musl
            override: true
      - name: Show rust versoins
        run: rustc -V
      - name: Make
        run: make

  build_1420:
    name: Build-1.42.0
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - name: Install toolchain
        uses: actions-rs/toolchain@v1
        with:
            toolchain: 1.42.0
            target: x86_64-unknown-linux-musl
            override: true
      - name: Show rust versoins
        run: rustc -V
      - name: Make
        run: make
  build_1390:
    name: Build-1.39.0
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - name: Install toolchain
        uses: actions-rs/toolchain@v1
        with:
            toolchain: 1.39.0
            target: x86_64-unknown-linux-musl
            override: true
      - name: Show rust versoins
        run: rustc -V
      - name: Maked
        run: make
  build_1320:
    name: Build-1.32.0
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - name: Install toolchain
        uses: actions-rs/toolchain@v1
        with:
            toolchain: 1.32.0
            target: x86_64-unknown-linux-musl
            override: true
      - name: Show rust versoins
        run: rustc -V
      - name: Make
        run: make
