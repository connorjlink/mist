# MIST
A DAP-compliant native Windows x86 process debugger designed to interface with my compiler, Haze, and its LSP-compliant editor, Clarity. Mist implements a static library callable by Haze, and it communicates with Clarity via WebSockets.

## RUNNING DIRECTIONS
> cargo run

## RELATED PROJECTS
- _haze_, my custom x86 and RISC-V optimizing C compiler designed to build Windows PE executables debuggable by _mist_.
  - [https://github.com/connorjlink/mist](https://github.com/connorjlink/mist)
- _clarity_, my scratch-built source code editor, LSP-compliant language server, and web-based data visualization tool for the Haze compiler.
  - [https://github.com/connorjlink/clarity](https://github.com/connorjlink/clarity)
