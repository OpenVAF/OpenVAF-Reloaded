use super::*;
use crate::grammar::expressions::expr;

pub(super) fn call(p: &mut Parser, lhs: CompletedMarker) -> CompletedMarker {
    let m = lhs.precede(p);
    arg_list(p);
    m.complete(p, CALL)
}

pub(super) fn sys_fun_call(p: &mut Parser) -> CompletedMarker {
    let m = p.start();
    let m2 = p.start();
    p.bump(SYSFUN);
    m2.complete(p, SYS_FUN);
    if p.at(T!('(')) {
        arg_list(p);
    }
    m.complete(p, CALL)
}

pub(super) fn arg_list(p: &mut Parser) {
    let m = p.start();
    p.eat(T!['(']);
    if !p.at(T![')']) && !p.at(EOF) {
        loop {
            // Every argument gets its own node so that a *null* argument
            // (`analog_expression_or_null`, A.6.5) keeps the position of the
            // arguments after it: `cross(V(d), dir, , , en)` passes `en` as the
            // fifth argument. Which functions may take a null argument, and in
            // which position, is checked in `syntax::validation`.
            let arg = p.start();
            if p.at(T![,]) || p.at(T![')']) {
                arg.complete(p, ARG);
            } else if expr(p).is_none() {
                arg.abandon(p);
                break;
            } else {
                arg.complete(p, ARG);
            }

            if p.at(T![')']) || p.at(EOF) || !p.expect(T![,]) {
                break;
            }
        }
    }
    p.eat(T![')']);
    m.complete(p, ARG_LIST);
}
