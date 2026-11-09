//! Eddie without a window. `Editor` is the whole editor: a front end draws
//! it and feeds it input, and `cli_main` drives it from the command line.

pub mod editor;
pub mod scripting;

pub use editor::{cli_main, Action, Editor, FrameEvents, NoPreview, PreviewSink, Wake};
