/**
 * A line-oriented tokenizer for the file pane's code viewer.
 *
 * The viewer draws one row per line with a sticky gutter, so highlighting has to
 * be produced a line at a time rather than over the whole blob: a token that
 * spanned rows would have to be split back apart anyway. Anything that survives
 * a newline — a block comment, a template literal, a Python docstring — is
 * carried forward in an explicit state value, which is also what makes the
 * function pure and testable one line at a time.
 *
 * This is a colourer, not a parser. It knows comments, strings, numbers, and
 * each language's reserved words, and nothing else: a highlighter that tries to
 * understand the grammar is wrong in ways the reader cannot predict, while one
 * that only marks lexical shapes is either right or plainly off.
 */

export type CodeTokenKind = "comment" | "string" | "number" | "keyword" | "plain";

export interface CodeToken {
  kind: CodeTokenKind;
  value: string;
}

/** What is still open at the end of a line, and what would close it. */
export interface CodeBlock {
  kind: "comment" | "string";
  close: string;
  /** Whether a backslash inside this block escapes the next character. */
  escape: boolean;
}

interface StringRule {
  open: string;
  close: string;
  escape: boolean;
  /** Whether the literal may stay open past the end of a line. */
  multiline: boolean;
}

interface CodeGrammar {
  lineComments: readonly string[];
  blockComments: readonly (readonly [string, string])[];
  strings: readonly StringRule[];
  keywords: ReadonlySet<string>;
  /** `<tag`, `</tag`, and `<?xml` read as markup rather than as comparisons. */
  markup: boolean;
  /**
   * Characters that belong to a word beyond `[A-Za-z0-9_$]`, and that may start
   * one. CSS needs `@` and `-` for `@font-face`, Ruby needs `?` and `!` for
   * `defined?`; giving those to every grammar would swallow `count - 1` whole.
   */
  wordExtra?: string;
}

function words(list: string): Set<string> {
  return new Set(list.split(" "));
}

/** `"` and `'`, escaped, ending at the line break. The shape most languages share. */
const QUOTES: readonly StringRule[] = [
  { open: "\"", close: "\"", escape: true, multiline: false },
  { open: "'", close: "'", escape: true, multiline: false }
];

const SLASH_COMMENTS = { lineComments: ["//"], blockComments: [["/*", "*/"]] } as const;

const JS_KEYWORDS = words(
  "as async await break case catch class const continue debugger default delete do else enum export "
  + "extends false finally for from function get if implements import in instanceof interface let new "
  + "null of package private protected public readonly return satisfies set static super switch this "
  + "throw true try type typeof undefined var void while with yield abstract declare infer keyof "
  + "namespace never unknown any boolean number object string symbol bigint"
);

const RUST_KEYWORDS = words(
  "as async await break const continue crate dyn else enum extern false fn for if impl in let loop "
  + "match mod move mut pub ref return self Self static struct super trait true type union unsafe use "
  + "where while bool char f32 f64 i8 i16 i32 i64 i128 isize str u8 u16 u32 u64 u128 usize String Vec "
  + "Option Some None Result Ok Err Box"
);

const GO_KEYWORDS = words(
  "break case chan const continue default defer else fallthrough for func go goto if import interface "
  + "map package range return select struct switch type var bool byte complex64 complex128 error float32 "
  + "float64 int int8 int16 int32 int64 rune string uint uint8 uint16 uint32 uint64 uintptr nil true false "
  + "make new len cap append copy delete panic recover"
);

const PYTHON_KEYWORDS = words(
  "and as assert async await break class continue def del elif else except finally for from global if "
  + "import in is lambda None nonlocal not or pass raise return True False try while with yield match case "
  + "self int str float bool list dict set tuple bytes"
);

const RUBY_KEYWORDS = words(
  "alias and begin break case class def defined? do else elsif end ensure false for if in module next nil "
  + "not or redo rescue retry return self super then true undef unless until when while yield attr_accessor "
  + "attr_reader attr_writer require require_relative include extend"
);

const JAVA_KEYWORDS = words(
  "abstract assert boolean break byte case catch char class const continue default do double else enum "
  + "extends final finally float for goto if implements import instanceof int interface long native new "
  + "package private protected public return short static strictfp super switch synchronized this throw "
  + "throws transient try var void volatile while true false null record sealed permits yield"
);

const KOTLIN_KEYWORDS = words(
  "abstract actual annotation as break by catch class companion const constructor continue crossinline "
  + "data delegate do dynamic else enum expect external false final finally for fun get if import in "
  + "infix init inline inner interface internal is lateinit noinline null object open operator out "
  + "override package private protected public reified return sealed set super suspend tailrec this "
  + "throw true try typealias val var vararg when where while Int Long Float Double Boolean String Unit Any"
);

const CSHARP_KEYWORDS = words(
  "abstract as async await base bool break byte case catch char checked class const continue decimal "
  + "default delegate do double else enum event explicit extern false finally fixed float for foreach "
  + "get goto if implicit in init int interface internal is lock long namespace new null object operator "
  + "out override params private protected public readonly record ref return sbyte sealed set short "
  + "sizeof stackalloc static string struct switch this throw true try typeof uint ulong unchecked "
  + "unsafe ushort using var virtual void volatile where while yield"
);

const SWIFT_KEYWORDS = words(
  "actor any as associatedtype async await break case catch class continue default defer deinit do else "
  + "enum extension fallthrough false fileprivate final for func guard if import in indirect init inout "
  + "internal is lazy let mutating nil none open operator private protocol public repeat rethrows return "
  + "self Self some static struct subscript super switch throw throws true try typealias var weak where "
  + "while Int Double Float String Bool Array Dictionary Set Optional"
);

const C_KEYWORDS = words(
  "alignas alignof auto bool break case catch char class concept const consteval constexpr continue "
  + "decltype default delete do double dynamic_cast else enum explicit export extern false float for "
  + "friend goto if inline int long mutable namespace new noexcept nullptr operator private protected "
  + "public register reinterpret_cast requires return short signed sizeof static static_assert "
  + "static_cast struct switch template this thread_local throw true try typedef typeid typename union "
  + "unsigned using virtual void volatile wchar_t while size_t uint8_t uint16_t uint32_t uint64_t "
  + "int8_t int16_t int32_t int64_t NULL include define ifdef ifndef endif pragma"
);

const PHP_KEYWORDS = words(
  "abstract and array as break callable case catch class clone const continue declare default do echo "
  + "else elseif empty enddeclare endfor endforeach endif endswitch endwhile enum extends final finally "
  + "fn for foreach function global goto if implements include include_once instanceof insteadof "
  + "interface isset list match namespace new or print private protected public readonly require "
  + "require_once return static switch throw trait try unset use var while xor yield true false null"
);

const LUA_KEYWORDS = words(
  "and break do else elseif end false for function goto if in local nil not or repeat return then true "
  + "until while self"
);

const SHELL_KEYWORDS = words(
  "if then elif else fi for while until do done case esac function in select time return break continue "
  + "local export readonly declare typeset unset source alias set shift trap exit eval exec echo printf "
  + "cd test true false param begin end try catch finally throw"
);

const SQL_KEYWORDS = words(
  "ADD ALL ALTER AND AS ASC BEGIN BETWEEN BY CASE CAST COLUMN COMMIT CONSTRAINT CREATE CROSS DEFAULT "
  + "DELETE DESC DISTINCT DROP ELSE END EXISTS FOREIGN FROM FULL GROUP HAVING IF IN INDEX INNER INSERT "
  + "INTO IS JOIN KEY LEFT LIKE LIMIT NOT NULL OFFSET ON OR ORDER OUTER PRIMARY REFERENCES RIGHT "
  + "ROLLBACK SELECT SET TABLE THEN TRANSACTION TRUE FALSE UNION UNIQUE UPDATE VALUES VIEW WHEN WHERE WITH"
);

const CSS_KEYWORDS = words(
  "@media @import @charset @keyframes @font-face @supports @namespace @page @layer @container @property "
  + "!important from to inherit initial unset revert none auto"
);

const JSON_KEYWORDS = words("true false null");

const YAML_KEYWORDS = words("true false null yes no on off");

const GRAPHQL_KEYWORDS = words(
  "query mutation subscription fragment on type input enum interface union scalar schema directive "
  + "implements extend true false null message service rpc returns repeated optional required package "
  + "syntax import option reserved oneof map"
);

/**
 * Every grammar the viewer knows.
 *
 * Longer string openers must come before their prefixes: `'''` has to be tried
 * before `'`, or a Python docstring reads as an empty string followed by prose.
 */
const GRAMMARS: Record<string, CodeGrammar> = {
  typescript: { ...SLASH_COMMENTS, markup: false, keywords: JS_KEYWORDS, strings: [
    { open: "`", close: "`", escape: true, multiline: true }, ...QUOTES
  ] },
  javascript: { ...SLASH_COMMENTS, markup: false, keywords: JS_KEYWORDS, strings: [
    { open: "`", close: "`", escape: true, multiline: true }, ...QUOTES
  ] },
  // A Rust string literal may span lines; a char literal may not, but the two
  // are not told apart lexically, so both are treated as the former.
  rust: { ...SLASH_COMMENTS, markup: false, keywords: RUST_KEYWORDS, strings: [
    { open: "\"", close: "\"", escape: true, multiline: true },
    { open: "'", close: "'", escape: true, multiline: false }
  ] },
  go: { ...SLASH_COMMENTS, markup: false, keywords: GO_KEYWORDS, strings: [
    { open: "`", close: "`", escape: false, multiline: true }, ...QUOTES
  ] },
  java: { ...SLASH_COMMENTS, markup: false, keywords: JAVA_KEYWORDS, strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true }, ...QUOTES
  ] },
  kotlin: { ...SLASH_COMMENTS, markup: false, keywords: KOTLIN_KEYWORDS, strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: false, multiline: true }, ...QUOTES
  ] },
  csharp: { ...SLASH_COMMENTS, markup: false, keywords: CSHARP_KEYWORDS, strings: QUOTES },
  swift: { ...SLASH_COMMENTS, markup: false, keywords: SWIFT_KEYWORDS, strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true }, ...QUOTES
  ] },
  c: { ...SLASH_COMMENTS, markup: false, keywords: C_KEYWORDS, strings: QUOTES },
  cpp: { ...SLASH_COMMENTS, markup: false, keywords: C_KEYWORDS, strings: QUOTES },
  php: {
    lineComments: ["//", "#"],
    blockComments: [["/*", "*/"]],
    markup: false,
    keywords: PHP_KEYWORDS,
    strings: QUOTES
  },
  python: {
    lineComments: ["#"],
    blockComments: [],
    markup: false,
    keywords: PYTHON_KEYWORDS,
    strings: [
      { open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true },
      { open: "'''", close: "'''", escape: true, multiline: true },
      ...QUOTES
    ]
  },
  ruby: {
    lineComments: ["#"],
    blockComments: [["=begin", "=end"]],
    markup: false,
    keywords: RUBY_KEYWORDS,
    wordExtra: "?!",
    strings: QUOTES
  },
  shell: {
    lineComments: ["#"],
    blockComments: [],
    markup: false,
    keywords: SHELL_KEYWORDS,
    // A single-quoted shell string takes no escapes at all: `'\'` is a backslash.
    strings: [
      { open: "\"", close: "\"", escape: true, multiline: false },
      { open: "'", close: "'", escape: false, multiline: false }
    ]
  },
  lua: {
    lineComments: ["--"],
    blockComments: [["--[[", "]]"]],
    markup: false,
    keywords: LUA_KEYWORDS,
    strings: [{ open: "[[", close: "]]", escape: false, multiline: true }, ...QUOTES]
  },
  sql: {
    lineComments: ["--"],
    blockComments: [["/*", "*/"]],
    markup: false,
    keywords: SQL_KEYWORDS,
    strings: [
      { open: "'", close: "'", escape: false, multiline: false },
      { open: "\"", close: "\"", escape: false, multiline: false }
    ]
  },
  css: {
    lineComments: [],
    blockComments: [["/*", "*/"]],
    markup: false,
    keywords: CSS_KEYWORDS,
    wordExtra: "@-!",
    strings: QUOTES
  },
  json: { lineComments: ["//"], blockComments: [["/*", "*/"]], markup: false, keywords: JSON_KEYWORDS, strings: QUOTES },
  yaml: { lineComments: ["#"], blockComments: [], markup: false, keywords: YAML_KEYWORDS, strings: QUOTES },
  toml: { lineComments: ["#", ";"], blockComments: [], markup: false, keywords: words("true false"), strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true },
    { open: "'''", close: "'''", escape: false, multiline: true },
    ...QUOTES
  ] },
  xml: { lineComments: [], blockComments: [["<!--", "-->"]], markup: true, keywords: new Set<string>(), strings: QUOTES },
  graphql: { lineComments: ["#", "//"], blockComments: [["/*", "*/"]], markup: false, keywords: GRAPHQL_KEYWORDS, strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true }, ...QUOTES
  ] }
};

export function grammarFor(language: string | null): CodeGrammar | null {
  if (language === null) return null;
  return GRAMMARS[language] ?? null;
}

function isWordCharacter(grammar: CodeGrammar, character: string): boolean {
  if (/[A-Za-z0-9_$]/.test(character)) return true;
  return grammar.wordExtra?.includes(character) ?? false;
}

/** A word may only start where an identifier could, so `2x` is a number then a word. */
function isWordStart(grammar: CodeGrammar, character: string): boolean {
  if (/[A-Za-z_$]/.test(character)) return true;
  return grammar.wordExtra?.includes(character) ?? false;
}

function isDigit(character: string): boolean {
  return character >= "0" && character <= "9";
}

/**
 * Index just past the closing delimiter, or -1 when it is not on this line.
 *
 * An escaped delimiter does not close: the backslash consumes whatever follows,
 * which is also why a line ending in an odd backslash inside an escaping string
 * keeps that string open.
 */
function findClose(line: string, from: number, close: string, escapes: boolean): number {
  let index = from;
  while (index < line.length) {
    if (escapes && line[index] === "\\") {
      index += 2;
      continue;
    }
    if (line.startsWith(close, index)) return index + close.length;
    index += 1;
  }
  return -1;
}

/**
 * Tokenizes one line, continuing whatever `block` left open.
 *
 * Adjacent plain characters are coalesced into a single token so a line of
 * punctuation does not become one DOM node per character.
 */
export function highlightCodeLine(
  grammar: CodeGrammar | null,
  line: string,
  block: CodeBlock | null
): { tokens: CodeToken[]; block: CodeBlock | null } {
  if (grammar === null) {
    return { tokens: line ? [{ kind: "plain", value: line }] : [], block: null };
  }

  const tokens: CodeToken[] = [];
  let plainFrom = 0;
  let index = 0;

  const flushPlain = (end: number) => {
    if (end > plainFrom) tokens.push({ kind: "plain", value: line.slice(plainFrom, end) });
  };
  const push = (kind: CodeTokenKind, from: number, to: number) => {
    flushPlain(from);
    tokens.push({ kind, value: line.slice(from, to) });
    plainFrom = to;
  };

  if (block) {
    const end = findClose(line, 0, block.close, block.escape);
    if (end < 0) {
      return { tokens: line ? [{ kind: block.kind, value: line }] : [], block };
    }
    tokens.push({ kind: block.kind, value: line.slice(0, end) });
    index = end;
    plainFrom = end;
  }

  while (index < line.length) {
    const character = line[index];

    const lineComment = grammar.lineComments.find((marker) => line.startsWith(marker, index));
    if (lineComment !== undefined) {
      push("comment", index, line.length);
      index = line.length;
      break;
    }

    const blockComment = grammar.blockComments.find(([open]) => line.startsWith(open, index));
    if (blockComment) {
      const end = findClose(line, index + blockComment[0].length, blockComment[1], false);
      if (end < 0) {
        push("comment", index, line.length);
        return { tokens, block: { kind: "comment", close: blockComment[1], escape: false } };
      }
      push("comment", index, end);
      index = end;
      continue;
    }

    const string = grammar.strings.find((rule) => line.startsWith(rule.open, index));
    if (string) {
      const end = findClose(line, index + string.open.length, string.close, string.escape);
      if (end < 0) {
        push("string", index, line.length);
        // An unterminated single-line literal is a typo, not a continuation: the
        // next line starts clean rather than colouring the rest of the file.
        return {
          tokens,
          block: string.multiline
            ? { kind: "string", close: string.close, escape: string.escape }
            : null
        };
      }
      push("string", index, end);
      index = end;
      continue;
    }

    if (grammar.markup && character === "<") {
      let cursor = index + 1;
      if (line[cursor] === "/") cursor += 1;
      const start = cursor;
      while (cursor < line.length && /[A-Za-z0-9_:.!?-]/.test(line[cursor])) cursor += 1;
      if (cursor > start) {
        push("keyword", index, cursor);
        index = cursor;
        continue;
      }
    }

    if (isDigit(character) && (index === 0 || !isWordCharacter(grammar, line[index - 1]))) {
      let cursor = index;
      while (cursor < line.length && /[0-9A-Fa-fxXoObB_.]/.test(line[cursor])) cursor += 1;
      push("number", index, cursor);
      index = cursor;
      continue;
    }

    if (isWordStart(grammar, character) && (index === 0 || !isWordCharacter(grammar, line[index - 1]))) {
      let cursor = index;
      while (cursor < line.length && isWordCharacter(grammar, line[cursor])) cursor += 1;
      const word = line.slice(index, cursor);
      // SQL reserves its words case-insensitively; every other grammar here does not.
      if (grammar.keywords.has(word) || grammar.keywords.has(word.toUpperCase())) {
        push("keyword", index, cursor);
      }
      index = cursor;
      continue;
    }

    index += 1;
  }

  flushPlain(line.length);
  return { tokens, block: null };
}

/**
 * Tokenizes a whole file, carrying block state from each line to the next.
 *
 * The viewer needs every line at once anyway — it draws them all — and doing the
 * carry here keeps the component from holding a mutable cursor across a render.
 */
export function highlightCodeLines(language: string | null, lines: readonly string[]): CodeToken[][] {
  const grammar = grammarFor(language);
  if (grammar === null) return lines.map((line) => (line ? [{ kind: "plain" as const, value: line }] : []));
  let block: CodeBlock | null = null;
  return lines.map((line) => {
    const result = highlightCodeLine(grammar, line, block);
    block = result.block;
    return result.tokens;
  });
}
