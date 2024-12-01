# Doom, ported to WASM, running in a terminal!

This project is based on the awesome work in [Wasm Doom](https://diekmann.github.io/wasm-fizzbuzz/doom/) by [Cornelius Diekmann](https://github.com/diekmann).
The inspiration for this project came from Ashton Meuser's [Doom in Godot](https://github.com/ashtonmeuser/godot-wasm-doom) project.

There is very little actual code in this project;
we set up Doom to run in Wasmer as usual,
then take the output from that and feed it to
[ratatui-image](https://docs.rs/ratatui-image) for display.

Since displaying images in terminals is somewhat... unstable,
you can switch protocols by pressing P to see which one works for you.

You can also zoom in and out with +/-.
I'm sure there's a way to get the image to scale correctly,
but I'm too lazy to find it! ╰(_°▽°_)╯

Doom uses the Ctrl, Shift and Alt keys for input.
Since those are modifier keys and reading them
in the terminal is sort of complicated,
the keys are remapped. The keys are
chosen so you use the same fingers as you normally would
when playing Doom. The mapping is:

- Ctrl -> Z
- Shift -> X
- Alt -> C
- Space -> V (Space itself works too, but this should make
  it less awkward to position your hand on the keyboard)
