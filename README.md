# Treasure Trove

<img src="static/logo.jpg" alt="Treasure Trove logo" width="200" align="center" /

A household ledger for tools and treasures:

- supports new or 20-year-old Zebra label printers via CUPS and ZPL
- single-file Rust backend
- SQLite database
- server-side rendered, pure HTML website
- text parsing handled by a ollama/ local LLM (optional)

## Overview

- accepts conversational, voice-to-text style input describing items you’re putting away.
- parses that text into structured items (name + quantity).
- stores items in SQLite with optional:
  - Container (bin, drawer, shelf, etc.)
  - Location (basement, workshop, etc.)
- prints labels to a Zebra printer 


## Usage

1. install Rust 
2. install SQLite 
3. clone the repository and `cd` into the project directory.
4. build and run:

   ```bash
   cargo run
   ```
