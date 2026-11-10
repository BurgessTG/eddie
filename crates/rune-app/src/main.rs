mod theme;

use rune::{AppSpec, Cx, El, col, text};

struct State;

fn view(_: &State, _: &mut Cx<State>) -> El {
    col()
        .w_full()
        .h_full()
        .padding(24.0)
        .child(text("Eddie").key("hello").help("A tooltip"))
}

fn main() {
    rune::run(AppSpec::new("Eddie", (640.0, 360.0), || State, view));
}
