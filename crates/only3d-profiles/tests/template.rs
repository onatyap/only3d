//! Placeholder-language semantics (mirrors Bambu Studio's PlaceholderParser).

use only3d_profiles::template::{self, eval_condition, render, Context, Template, TemplateErrorKind, VarForm};

fn ctx() -> Context {
    let mut c = Context::new();
    c.set("i", 7i64)
        .set("j", 2i64)
        .set("x", 12.2)
        .set("zero", 0i64)
        .set("yes", true)
        .set("no", false)
        .set("name", "PLA")
        .set("temps", vec![220i64, 245, 260])
        .set("speeds", vec![25.0, 40.0])
        .set("one_float", vec![0.4])
        .set("types", vec!["PLA", "PETG"])
        .set("flags", vec![true, false])
        .set("filament_map", vec![2i64, 1])
        .set("idx", 1i64)
        .set("fidx", 1.0);
    c
}

fn r(t: &str) -> String {
    render(t, &ctx()).unwrap_or_else(|e| panic!("{e}"))
}

fn err(t: &str) -> template::TemplateError {
    render(t, &ctx()).expect_err("expected an error")
}

#[test]
fn arithmetic_and_formatting() {
    assert_eq!(r("{i / j}"), "3"); // int division
    assert_eq!(r("{i % j}"), "1");
    assert_eq!(r("{i / 2.0}"), "3.5");
    assert_eq!(r("{x + 8.0}"), "20.2");
    assert_eq!(r("{x + 0.4}"), "12.6");
    assert_eq!(r("{+0.0}"), "0");
    assert_eq!(r("{-0.01}"), "-0.01");
    assert_eq!(r("{-i}"), "-7");
    assert_eq!(r("{25/2.4053*60*0.8}"), "498.898");
    assert_eq!(r("{1000000.0}"), "1e+06");
    assert_eq!(r("{2 + 3 * 4 - (1 + 1)}"), "12");
    assert_eq!(r("{name + \"-\" + i}"), "PLA-7");
    assert_eq!(r("{i + name}"), "7PLA");
    assert_eq!(r("{(i > j)}"), "true");
    assert_eq!(r("{yes}"), "true");
    assert_eq!(r("[yes] [no] [i] [x] [name]"), "1 0 7 12.2 PLA");
    assert_eq!(err("{i / zero}").kind, TemplateErrorKind::Eval("division by zero".into()));
}

#[test]
fn functions() {
    assert_eq!(r("{max(i, 200)} {min(x, 3)} {max(1, 2.5)}"), "200 3 2.5");
    assert_eq!(r("{ceil(250 / 125.0) * 5} {ceil(2.1)} {floor(-2.1)} {round(2.5)} {int(-2.7)}"), "10 3 -3 3 -2");
    assert_eq!(r("{digits(x, 6, 2)}|{zdigits(x, 6, 2)}|{digits(i, 3)}|{zdigits(i, 3)}"), " 12.20|012.20|  7|007");
    assert_eq!(r("{filament_change(1)}[temps]"), "245");
}

#[test]
fn vectors_and_legacy_expansion() {
    let mut c = ctx();
    assert_eq!(render("[temps]", &c).unwrap(), "220"); // current filament defaults to 0
    c.set_current_filament_id(2);
    assert_eq!(render("[temps] [speeds] [types]", &c).unwrap(), "260 25 PLA"); // out of range -> [0]
    assert_eq!(r("[temps_1] [temps[idx]] {temps[idx]} {temps[idx + 1]} {temps[-1]} {temps[9]}"), "245 245 245 260 220 220");
    assert_eq!(r("{types[1]} {flags[0]} [flags[idx]]"), "PETG true 0");
    assert_eq!(r("{speeds[filament_map[0] - 1]}"), "40");
    // Scalar use of a float vector: element 0 for one-element vectors, else Bambu's
    // filament_map[current filament] quirk (here 2 -> out of range -> element 0).
    assert_eq!(r("{one_float} {speeds}"), "0.4 25");
    assert!(matches!(err("{temps}").kind, TemplateErrorKind::Eval(_)));
    assert!(matches!(err("{temps[fidx]}").kind, TemplateErrorKind::Eval(_)));
    assert!(matches!(err("{i[0]}").kind, TemplateErrorKind::Eval(_)));
    let mut nil = Context::new();
    nil.set("v", vec![f64::NAN, 1.5]);
    assert_eq!(render("[v] {v[1]}", &nil).unwrap(), "nil 1.5");
}

#[test]
fn conditions() {
    let t = "a{if i > 5}\n  big\n{elsif i > 2}\n  mid\n{else}\n  small\n{endif}b";
    assert_eq!(r(t), "a\n  big\nb");
    assert_eq!(r("{if no}x{elsif yes && i == 7}y{endif}"), "y");
    assert_eq!(r("{if name == \"PLA\" and not no}ok{endif}"), "ok");
    assert_eq!(r("{if (name == \"PETG\")\n || (types[1] == \"PETG\")}multi-line{endif}"), "multi-line");
    assert_eq!(r("{if !(yes) || i != 7}1{else}2{endif}"), "2");
    assert_eq!(r("{if x == 12.2}eq{endif}{if 0.1 + 0.2 == 0.3}tol{endif}"), "eqtol");
    assert_eq!(r("{if idx <> 0}ne{endif}{if i >= 7 && i <= 7}rng{endif}"), "nerng");
    assert_eq!(r("{if 1 == 1}{if no}A{else}{if yes}B{endif}{endif}{endif}"), "B");
    assert_eq!(r("{(yes ? x : 0.0) + 1}"), "13.2");
    assert_eq!(r("{if \"abc\" < \"abd\"}lt{endif}"), "lt");
    assert_eq!(r("{if idx == \"1\"}strnum{endif}"), "strnum"); // string vs number compares strings
    // Conditions must be booleans.
    assert!(matches!(err("{if i}x{endif}").kind, TemplateErrorKind::Eval(_)));
    assert!(matches!(err("{if yes && i}x{endif}").kind, TemplateErrorKind::Eval(_)));
}

#[test]
fn whitespace_rules() {
    // Leading whitespace of the whole template is dropped (Boost.Spirit pre-skip), the rest kept.
    assert_eq!(r("\n\n  G1 X[i]\n"), "G1 X7\n");
    assert_eq!(r("{ i }[ i ]{\n  i\n}"), "777");
    assert_eq!(r("{if yes}\n    M106 S255\n{endif}\n"), "\n    M106 S255\n\n");
}

#[test]
fn every_branch_is_evaluated() {
    // Unknown variables in branches that are not taken are still errors (as in Bambu).
    let e = err("ok\n{if no}\nG1 X{missing_var}\n{endif}");
    assert_eq!(e.unknown_variable(), Some("missing_var"));
    assert_eq!(e.line, 3);
    assert_eq!(e.line_text, "G1 X{missing_var}");
    assert!(e.to_string().contains("line 3") && e.to_string().contains("missing_var"));
    // No short-circuit either.
    assert_eq!(err("{if no && also_missing}x{endif}").unknown_variable(), Some("also_missing"));
    assert_eq!(err("{(yes ? 1 : ghost)}").unknown_variable(), Some("ghost"));
    assert_eq!(err("M620 S[nope]A").unknown_variable(), Some("nope"));
    assert_eq!(err("[temps[nope]]").unknown_variable(), Some("nope"));
}

#[test]
fn syntax_errors() {
    for (t, line) in [
        ("{if yes}x", 1),
        ("a\n{endif}", 2),
        ("{i == 7}", 1),
        ("[ ]", 1),
        ("[0]", 1),
        ("{max(1)}", 1),
        ("{\"unterminated}", 1),
        ("{if yes}a{else}b{elsif no}c{endif}", 1),
        ("line1\nline2 {i +}", 2),
    ] {
        let e = Template::parse(t).expect_err(t);
        assert!(matches!(e.kind, TemplateErrorKind::Syntax(_)), "{t}: {e}");
        assert_eq!(e.line, line, "{t}: {e}");
    }
    let e = Template::parse("{i == 7}").unwrap_err();
    assert!(e.to_string().contains("parenthesise"), "{e}");
}

#[test]
fn required_variables_and_missing() {
    let src = "{if a > 1}[b]{else}{c[d + 1]}{endif}[e[f]][g_2]{min(h, 1)}";
    let t = Template::parse(src).unwrap();
    assert_eq!(t.required_variables(), ["a", "b", "c", "d", "e", "f", "g_2", "h"]);
    let uses = t.variable_uses();
    assert!(uses.iter().any(|u| u.name == "c" && u.form == VarForm::Indexed));
    assert!(uses.iter().any(|u| u.name == "f" && u.form == VarForm::LegacyIndexVar));
    let mut c = Context::new();
    c.set("a", 1i64).set("g", vec![1i64, 2, 3]);
    assert_eq!(t.missing_variables(&[&c]), ["b", "c", "d", "e", "f", "h"]);
    assert_eq!(template::required_variables("plain text").unwrap(), Vec::<String>::new());
}

#[test]
fn layered_contexts() {
    let base = ctx();
    let mut over = Context::new();
    over.set("i", 100i64).set_current_filament_id(1);
    let t = Template::parse("{i} [temps] {j}").unwrap();
    assert_eq!(t.render_layered(&[&over, &base]).unwrap(), "100 245 2");
    assert_eq!(t.render(&base).unwrap(), "7 220 2");
}

#[test]
fn boolean_conditions_and_regex() {
    let c = ctx();
    assert!(eval_condition("name == \"PLA\" and temps[1] > 240", &c).unwrap());
    assert!(eval_condition("name =~ /P.A/", &c).unwrap());
    assert!(!eval_condition("name !~ /PL.*/", &c).unwrap());
    assert!(eval_condition("types[1] =~ /(PLA|PETG)(-CF)?/", &c).unwrap());
    assert!(eval_condition("i", &c).is_err());
}

/// PrusaSlicer's and OrcaSlicer's macro statements, which other makers' templates use.
#[test]
fn statements_variables_and_tables() {
    // locals, assignment, several statements in one macro; a statement writes nothing
    assert_eq!(r("{local a = i * 2; a = a + 1; a}"), "15");
    assert_eq!(r("{local t = temps[1]}T{t}"), "T245");
    // lists, repeat and element assignment
    assert_eq!(r("{local v = (1, 2.5, 4); v[2]}"), "4");
    assert_eq!(r("{local r = repeat(3, 0); r[1] = 5; r[1] + r[0]}"), "5");
    // assigning to something the slicer owns (PrusaSlicer's writer state) just keeps it
    assert_eq!(r("{e_retracted[0] = 1.5 * x}ok"), "ok");
    // functions
    assert_eq!(r("{interpolate_table(5, (0, 0), (10, 100))}"), "50");
    assert_eq!(r("{interpolate_table(20, (0, 0), (10, 100))}"), "100");
    assert_eq!(r("{size(temps)}"), "3");
    assert_eq!(r("{if one_of(name, \"ABS\", \"PLA\")}yes{endif}"), "yes");
    assert_eq!(r("{if is_nil(speeds[0])}nil{else}set{endif}"), "set");
    // a `global` carries over to the next template of the same job
    let mut globals = Default::default();
    let c = ctx();
    let a = Template::parse("{global g = i + 1}").unwrap().render_with_globals(&[&c], &mut globals).unwrap();
    assert_eq!(a, "");
    assert_eq!(Template::parse("G{g}").unwrap().render_with_globals(&[&c], &mut globals).unwrap(), "G8");
    // a local doesn't
    assert!(Template::parse("{local l = 1}").unwrap().render_with_globals(&[&c], &mut globals).is_ok());
    assert!(Template::parse("{l}").unwrap().render_with_globals(&[&c], &mut globals).is_err());
}

/// PrusaSlicer's `if ... then ... endif` inside a macro, `~vector` in one_of, and a block of
/// statements spread over lines (Snapmaker U1, Prusa XL and CORE One INDX templates).
#[test]
fn statement_if_and_vector_choices() {
    assert_eq!(r("{ if (0 <= i && i < 3) then local s = i else local s = 3 endif}; s={s}"), "; s=3");
    assert_eq!(r("{if j > 1 then local t = 5; elsif j > 0 then local t = 1; endif}{t}"), "5");
    let block = "{\nlocal z = 10;\n\nif x < 20 then\n    z = z + 2;\nendif\n}\nG1 Z{z}";
    assert_eq!(r(block), "\nG1 Z12");
    assert_eq!(r("{\"G1 F\" + (i * 60) + \"\"}"), "G1 F420");
    let mut c = ctx();
    c.set("low", vec!["PLA", "PETG"]);
    assert_eq!(render("{if one_of(name, ~low)}cool{endif}", &c).unwrap(), "cool");
}
