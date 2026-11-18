use mindspace::core::{CodeEntityKind as K, CodeLanguage as L, CodeParseStatus};
use mindspace::ingest::code::{self, ParsePolicy};

#[test]
fn go_and_ecmascript_emit_declarations_from_their_grammars() {
    for (language, source, expected) in [
        (L::Go, "package sample\ntype Bag[T any] struct { Left, Right T }\nfunc (b *Bag[T]) Take() {}\ntype Port interface { Read() }\ntype Label string\ntype Alias = string\nconst A, B = 1, 2\n", vec![
            ("Bag", K::Struct), ("Bag.Left", K::Field), ("Bag.Right", K::Field), ("Bag.Take", K::Method),
            ("Port", K::Interface), ("Port.Read", K::Method), ("Label", K::TypeDefinition), ("Alias", K::TypeAlias),
            ("A", K::Constant), ("B", K::Constant),
        ]),
        (L::TypeScript, "interface Config { key: string; open(): void }\ntype Items = Config[];\nexport class Reader { async load() {} }\nconst pick = async (x: number) => x;\n", vec![
            ("Config", K::Interface), ("Config.key", K::Field), ("Config.open", K::Method), ("Items", K::TypeAlias),
            ("Reader", K::Class), ("Reader.load", K::Method), ("pick", K::Function),
        ]),
        (L::Tsx, "export const View = (p: {n: number}) => <span>{p.n}</span>;", vec![("View", K::Function)]),
        (L::JavaScript, "export class Reader { #cache; load() { return <main/>; } }\nexport const Box = () => <aside/>;", vec![
            ("Reader", K::Class), ("Reader.#cache", K::Field), ("Reader.load", K::Method), ("Box", K::Function),
        ]),
    ] {
        let d = code::parse(language, source);
        assert_eq!(d.status, CodeParseStatus::Complete, "{language:?} {d:?}");
        let found: Vec<_> = d.entities.iter().map(|e| (e.qualified_name.as_deref().unwrap(), e.kind)).collect();
        assert_eq!(found, expected, "{language:?}");
        for e in d.entities {
            assert!(source[e.range.start_byte as usize..e.range.end_byte as usize].contains(e.name.as_deref().unwrap()));
            if language == L::TypeScript && matches!(e.name.as_deref(), Some("load" | "pick")) {
                assert!(e.is_async);
            }
        }
    }
}

#[test]
fn svelte_cites_inline_scripts_at_original_byte_and_line_positions() {
    let source = r#"<!-- <script>function Comment() {}</script> -->
<p>é</p><script module>export function Outer() {}</script>
<script lang='ts'>
  const Inner = (s: string) => s;
</script>
<div><script>function Nested() {}</script></div>
<script lang="coffee">function Hidden() {}</script>
<script type="application/json">{"name":"Ignored"}</script>
<script src="./external.js">function Outside() {}</script>
<style>p { color: red; }</style>"#;
    let d = code::parse(L::Svelte, source);
    assert_eq!(d.status, CodeParseStatus::Complete, "{d:?}");
    assert_eq!(d.entities.len(), 2, "{:?}", d.entities);
    for (e, (span, line, column)) in d.entities.iter().zip([
        ("function Outer() {}", 2, 32),
        ("Inner = (s: string) => s", 4, 9),
    ]) {
        assert_eq!(
            &source[e.range.start_byte as usize..e.range.end_byte as usize],
            span
        );
        assert_eq!((e.range.start_line, e.range.start_column), (line, column));
    }
    assert_eq!(
        code::parse(L::Svelte, "<script>function broken(</script>").status,
        CodeParseStatus::Recovered
    );
}

#[test]
fn language_specific_skip_identity_and_aggregate_script_budget() {
    let source = "<script module>function A() {}</script>\n<script>function B() {}</script>";
    let full = code::parse_with_policy(L::Svelte, source, &ParsePolicy::default());
    assert_eq!(full.derivation.entities.len(), 2);
    for policy in [
        ParsePolicy {
            max_source_bytes: 2,
            ..ParsePolicy::default()
        },
        ParsePolicy {
            max_entity_depth: 0,
            ..ParsePolicy::default()
        },
        ParsePolicy {
            max_output_bytes: full.accounted_bytes - 1,
            ..ParsePolicy::default()
        },
    ] {
        let outcome = code::parse_with_policy(L::Svelte, source, &policy);
        let reason = outcome.skipped.unwrap();
        assert_eq!(
            outcome.derivation.identity,
            code::skip_identity_for(L::Svelte, reason)
        );
        assert!(outcome.derivation.entities.is_empty());
    }
}
