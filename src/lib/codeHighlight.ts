/**
 * A line-oriented tokenizer for the code the app shows: the file pane's viewer
 * and fenced blocks in rendered Markdown.
 *
 * The viewer draws one row per line with a gutter, so highlighting has to be
 * produced a line at a time rather than over the whole blob: a token that spanned
 * rows would have to be split back apart anyway. Anything that survives a newline
 * — a block comment, a template literal, a Python docstring, a markup tag whose
 * attributes run on — is carried forward in an explicit state value, which is
 * also what makes the function pure and testable one line at a time.
 *
 * This is a colourer, not a parser. It knows lexical shapes — comments, strings,
 * numbers, each language's reserved words — and a handful of shapes that are just
 * as local: a word directly followed by `(` is a call, a capitalised word is a
 * type name, `@name` is a decorator, `"key":` is a key. Nothing here looks further
 * than the line it is on, so it is either right or plainly off, never subtly wrong
 * in a way the reader cannot predict.
 */

export type CodeTokenKind =
  | "comment"
  | "string"
  | "number"
  | "keyword"
  | "function"
  | "type"
  | "meta"
  | "tag"
  | "attribute"
  | "property"
  | "variable"
  | "inserted"
  | "deleted"
  | "plain";

export interface CodeToken {
  kind: CodeTokenKind;
  value: string;
}

/**
 * What is still open at the end of a line, and what would close it.
 *
 * `tag` is the inside of a markup tag whose attributes continue on the next line:
 * nothing is coloured wholesale, but a word followed by `=` there is an attribute.
 */
export interface CodeBlock {
  kind: "comment" | "string" | "tag";
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
  /** Comment markers that only count as the first thing on a line: batch `REM`, `::`. */
  leadingComments?: readonly string[];
  blockComments: readonly (readonly [string, string])[];
  strings: readonly StringRule[];
  keywords: ReadonlySet<string>;
  /** Reserved words match whatever their case (SQL, BASIC, batch); the set holds them upper-cased. */
  caseInsensitive?: boolean;
  /** `<tag`, `</tag`, and `<?xml` read as markup rather than as comparisons. */
  markup: boolean;
  /**
   * JSX: `<Tag` is markup only where an expression could start, so `a < b` and
   * `Array<string>` stay what they are.
   */
  jsx?: boolean;
  /**
   * Characters that belong to a word beyond `[A-Za-z0-9_$]`, and that may start
   * one. CSS needs `@` and `-` for `@font-face`, Ruby needs `?` and `!` for
   * `defined?`; giving those to every grammar would swallow `count - 1` whole.
   */
  wordExtra?: string;
  /** A word directly followed by `(` is a call. Off where `(` means something else (Lisp). */
  calls: boolean;
  /** A capitalised word — `HashMap`, `Promise` — names a type. */
  types: boolean;
  /** `@name` is an annotation or a decorator. */
  decorators?: boolean;
  /** Characters that open a variable reference: `$` in the shell, PHP, and Perl. */
  sigils?: string;
  /** `#include`: a line whose first character is this is a preprocessor directive. */
  directive?: string;
  /** `#[derive]`: an attribute written as a bracketed run after one of these. */
  attributeOpeners?: readonly string[];
  /** `println!(`: a word ending in `!` before a bracket is a macro call. */
  macros?: boolean;
  /** `\section`: a command prefix, as in TeX. */
  commandPrefix?: string;
  /** `#fff` in a stylesheet is a colour, not a selector. */
  hexColors?: boolean;
  /**
   * How a key is written at the start of a line: JSON's `"key":`, YAML's `key:`,
   * TOML's and INI's `key =`, a stylesheet's `property:`.
   */
  keys?: "quoted" | "colon" | "equals" | "css";
  /** `[section]` on a line of its own is a heading (TOML, INI). */
  sections?: boolean;
  /** Whole-line grammars, where the first characters decide the line. */
  lines?: "diff" | "markdown" | "log";
}

function words(list: string): Set<string> {
  return new Set(list.split(/\s+/).filter(Boolean));
}

function upperWords(list: string): Set<string> {
  return new Set(list.split(/\s+/).filter(Boolean).map((word) => word.toUpperCase()));
}

/** `"` and `'`, escaped, ending at the line break. The shape most languages share. */
const QUOTES: readonly StringRule[] = [
  { open: "\"", close: "\"", escape: true, multiline: false },
  { open: "'", close: "'", escape: true, multiline: false }
];

const DOUBLE_QUOTE: readonly StringRule[] = [{ open: "\"", close: "\"", escape: true, multiline: false }];

const TRIPLE_QUOTES: readonly StringRule[] = [
  { open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true },
  { open: "'''", close: "'''", escape: true, multiline: true }
];

const SLASH_COMMENTS = { lineComments: ["//"], blockComments: [["/*", "*/"]] } as const;

const HASH_COMMENTS = { lineComments: ["#"], blockComments: [] } as const;

/** The defaults a C-family grammar starts from; each entry below states only how it differs. */
const CODE = { markup: false, calls: true, types: true } as const;

/** Data and configuration: nothing in them is a call or a type. */
const DATA = { markup: false, calls: false, types: false } as const;

const JS_KEYWORDS = words(
  "as async await break case catch class const continue debugger default delete do else enum export "
  + "extends false finally for from function get if implements import in instanceof interface let new "
  + "null of package private protected public readonly return satisfies set static super switch this "
  + "throw true try type typeof undefined var void while with yield abstract declare infer keyof "
  + "namespace never unknown any boolean number object string symbol bigint override accessor"
);

const RUST_KEYWORDS = words(
  "as async await break const continue crate dyn else enum extern false fn for if impl in let loop "
  + "match mod move mut pub ref return self Self static struct super trait true type union unsafe use "
  + "where while bool char f32 f64 i8 i16 i32 i64 i128 isize str u8 u16 u32 u64 u128 usize"
);

const GO_KEYWORDS = words(
  "break case chan const continue default defer else fallthrough for func go goto if import interface "
  + "map package range return select struct switch type var bool byte complex64 complex128 error float32 "
  + "float64 int int8 int16 int32 int64 rune string uint uint8 uint16 uint32 uint64 uintptr nil true false "
  + "any comparable iota"
);

const PYTHON_KEYWORDS = words(
  "and as assert async await break class continue def del elif else except finally for from global if "
  + "import in is lambda None nonlocal not or pass raise return True False try while with yield match case "
  + "self cls type int str float bool list dict set tuple bytes object"
);

const RUBY_KEYWORDS = words(
  "alias and begin break case class def defined? do else elsif end ensure false for if in module next nil "
  + "not or redo rescue retry return self super then true undef unless until when while yield attr_accessor "
  + "attr_reader attr_writer require require_relative include extend private protected public lambda proc"
);

const JAVA_KEYWORDS = words(
  "abstract assert boolean break byte case catch char class const continue default do double else enum "
  + "extends final finally float for goto if implements import instanceof int interface long native new "
  + "package private protected public return short static strictfp super switch synchronized this throw "
  + "throws transient try var void volatile while true false null record sealed permits yield non-sealed"
);

const KOTLIN_KEYWORDS = words(
  "abstract actual annotation as break by catch class companion const constructor continue crossinline "
  + "data delegate do dynamic else enum expect external false final finally for fun get if import in "
  + "infix init inline inner interface internal is lateinit noinline null object open operator out "
  + "override package private protected public reified return sealed set super suspend tailrec this "
  + "throw true try typealias val var vararg when where while value"
);

const CSHARP_KEYWORDS = words(
  "abstract as async await base bool break byte case catch char checked class const continue decimal "
  + "default delegate do double dynamic else enum event explicit extern false finally fixed float for "
  + "foreach get goto if implicit in init int interface internal is lock long namespace new null object "
  + "operator out override params partial private protected public readonly record ref required return "
  + "sbyte sealed set short sizeof stackalloc static string struct switch this throw true try typeof "
  + "uint ulong unchecked unsafe ushort using var virtual void volatile when where while with yield"
);

const SWIFT_KEYWORDS = words(
  "actor any as associatedtype async await break case catch class continue convenience default defer "
  + "deinit do else enum extension fallthrough false fileprivate final for func guard if import in "
  + "indirect init inout internal is lazy let mutating nil none nonisolated open operator override "
  + "private protocol public repeat required rethrows return self Self some static struct subscript "
  + "super switch throw throws true try typealias var weak where while"
);

const C_KEYWORDS = words(
  "alignas alignof auto bool break case catch char char8_t char16_t char32_t class concept const consteval "
  + "constexpr constinit continue co_await co_return co_yield decltype default delete do double "
  + "dynamic_cast else enum explicit export extern false final float for friend goto if inline int long "
  + "mutable namespace new noexcept nullptr operator override private protected public register "
  + "reinterpret_cast requires restrict return short signed sizeof static static_assert static_cast "
  + "struct switch template this thread_local throw true try typedef typeid typename union unsigned "
  + "using virtual void volatile wchar_t while size_t ssize_t uint8_t uint16_t uint32_t uint64_t int8_t "
  + "int16_t int32_t int64_t uintptr_t intptr_t NULL _Bool _Complex _Atomic _Generic _Noreturn"
);

const OBJC_KEYWORDS = new Set([
  ...C_KEYWORDS,
  ...words("id self super nil Nil YES NO BOOL SEL IMP instancetype nonatomic atomic strong weak copy "
    + "assign readonly readwrite nullable nonnull")
]);

/** GPU shading languages share C's shape and add their own vocabulary of vectors and qualifiers. */
const SHADER_KEYWORDS = new Set([
  ...C_KEYWORDS,
  ...words("uniform varying attribute in out inout layout precision highp mediump lowp vec2 vec3 vec4 "
    + "ivec2 ivec3 ivec4 uvec2 uvec3 uvec4 bvec2 bvec3 bvec4 mat2 mat3 mat4 sampler2D samplerCube "
    + "discard float2 float3 float4 float4x4 half half2 half3 half4 cbuffer Texture2D SamplerState "
    + "fn let var struct vec2f vec3f vec4f mat4x4f f32 i32 u32 kernel device constant threadgroup "
    + "__global__ __device__ __host__ __shared__ __constant__")
]);

const PHP_KEYWORDS = words(
  "abstract and array as break callable case catch class clone const continue declare default do echo "
  + "else elseif empty enddeclare endfor endforeach endif endswitch endwhile enum extends final finally "
  + "fn for foreach function global goto if implements include include_once instanceof insteadof "
  + "interface isset list match namespace new or print private protected public readonly require "
  + "require_once return static switch throw trait try unset use var while xor yield true false null "
  + "self parent"
);

const LUA_KEYWORDS = words(
  "and break do else elseif end false for function goto if in local nil not or repeat return then true "
  + "until while self"
);

const SHELL_KEYWORDS = words(
  "if then elif else fi for while until do done case esac function in select time return break continue "
  + "local export readonly declare typeset unset source alias set shift trap exit eval exec echo printf "
  + "cd test true false read wait"
);

const POWERSHELL_KEYWORDS = upperWords(
  "begin break catch class continue data define do dynamicparam else elseif end enum exit filter "
  + "finally for foreach from function if in param process return switch throw trap try until using "
  + "var while workflow -eq -ne -gt -ge -lt -le -like -notlike -match -notmatch -contains -notcontains "
  + "-in -notin -and -or -not -xor -is -isnot -as -replace -split -join"
);

const BATCH_KEYWORDS = upperWords(
  "echo set if else for in do goto call exit not exist defined errorlevel setlocal endlocal pause shift "
  + "start cd chdir copy del move mkdir md rmdir rd type equ neq lss leq gtr geq on off nul cls title "
  + "enabledelayedexpansion"
);

const SQL_KEYWORDS = upperWords(
  "add all alter and any as asc begin between by case cast check column commit constraint create cross "
  + "database default delete desc distinct drop else end except exists foreign from full function group "
  + "having if in index inner insert intersect into is join key left like limit merge natural not null "
  + "offset on or order outer over partition primary procedure references replace returning right "
  + "rollback row rows schema select set table then to transaction trigger true false truncate union "
  + "unique update using values view when where window with int integer bigint smallint text varchar "
  + "char boolean date timestamp numeric decimal real serial"
);

const VB_KEYWORDS = upperWords(
  "and andalso as boolean byref byte byval call case catch class const date dim do double each else "
  + "elseif end enum error exit false finally for friend function get goto handles if implements imports "
  + "in inherits integer interface is let lib like long loop me mod module mybase namespace new next not "
  + "nothing object of on option optional or orelse overrides paramarray private property protected "
  + "public raiseevent readonly redim resume return select set shared short single static step stop "
  + "string structure sub then throw to true try typeof until variant wend when while with withevents"
);

const FORTRAN_KEYWORDS = upperWords(
  "program end module use implicit none integer real double precision complex logical character "
  + "parameter dimension allocatable intent in out inout subroutine function call return if then else "
  + "elseif endif do enddo while select case default contains type print write read stop cycle exit "
  + "allocate deallocate result pure elemental recursive interface procedure public private"
);

const VHDL_KEYWORDS = upperWords(
  "entity is port end architecture of begin signal process if then else elsif case when others library "
  + "use all in out inout std_logic std_logic_vector downto to component map generic constant variable "
  + "type function return and or not xor nand nor rising_edge falling_edge generate loop for while wait "
  + "until after report severity integer natural boolean"
);

const CMAKE_KEYWORDS = upperWords(
  "if else elseif endif foreach endforeach while endwhile function endfunction macro endmacro set unset "
  + "project add_executable add_library target_link_libraries target_include_directories "
  + "target_compile_definitions target_compile_options include find_package message option return "
  + "cmake_minimum_required install add_subdirectory add_custom_command add_custom_target list string "
  + "file get_filename_component configure_file include_directories link_directories"
);

const DOCKER_KEYWORDS = upperWords(
  "from as run cmd label maintainer expose env add copy entrypoint volume user workdir arg onbuild "
  + "stopsignal healthcheck shell"
);

const DART_KEYWORDS = words(
  "abstract as assert async await base break case catch class const continue covariant default deferred "
  + "do dynamic else enum export extends extension external factory false final finally for get hide if "
  + "implements import in interface is late library mixin new null on operator part required rethrow "
  + "return sealed set show static super switch sync this throw true try typedef var void when while "
  + "with yield int double num bool"
);

const SCALA_KEYWORDS = words(
  "abstract case catch class def do else enum export extends false final finally for forSome given if "
  + "implicit import lazy match new null object override package private protected return sealed super "
  + "then this throw trait true try type using val var while with yield"
);

const GROOVY_KEYWORDS = new Set([...JAVA_KEYWORDS, ...words("def in as trait it")]);

const R_KEYWORDS = words(
  "if else repeat while function for in next break TRUE FALSE NULL Inf NaN NA NA_integer_ NA_real_ "
  + "NA_character_ library require return"
);

const JULIA_KEYWORDS = words(
  "abstract baremodule begin break catch const continue do else elseif end export false finally for "
  + "function global if import in let local macro module mutable primitive quote return struct true try "
  + "type using where while nothing missing"
);

const HASKELL_KEYWORDS = words(
  "as case class data default deriving do else family forall foreign hiding if import in infix infixl "
  + "infixr instance let mdo module newtype of proc qualified rec then type where"
);

const ELIXIR_KEYWORDS = words(
  "after alias and case catch cond def defdelegate defexception defguard defimpl defmacro defmodule defp "
  + "defprotocol defstruct do else end false fn for if import in nil not or quote raise receive require "
  + "rescue true try unless unquote use when with"
);

const ERLANG_KEYWORDS = words(
  "after and andalso band begin bnot bor bsl bsr bxor case catch cond div end fun if let not of or orelse "
  + "receive rem try when xor true false"
);

const LISP_KEYWORDS = words(
  "def defn defn- defmacro defmulti defmethod defprotocol defrecord deftype defonce fn let letfn loop "
  + "recur if if-not if-let when when-not when-let cond condp case do doseq dotimes for ns require import "
  + "use quote try catch finally throw nil true false and or not define lambda let* letrec begin set! "
  + "else defun defvar defparameter setf setq progn"
);

const ZIG_KEYWORDS = words(
  "addrspace align allowzero and anyframe anytype asm async await break callconv catch comptime const "
  + "continue defer else enum errdefer error export extern fn for if inline linksection noalias noinline "
  + "nosuspend opaque or orelse packed pub resume return struct suspend switch test threadlocal try union "
  + "unreachable usingnamespace var volatile while true false null undefined void bool u8 u16 u32 u64 "
  + "usize i8 i16 i32 i64 isize f32 f64 type anyerror noreturn"
);

const NIM_KEYWORDS = words(
  "addr and as asm bind block break case cast concept const continue converter defer discard distinct "
  + "div do elif else end enum except export finally for from func if import in include interface is "
  + "isnot iterator let macro method mixin mod nil not notin object of or out proc ptr raise ref return "
  + "shl shr static template try tuple type using var when while xor yield true false"
);

const PERL_KEYWORDS = words(
  "my our local sub if elsif else unless while until for foreach do last next redo return use require "
  + "package no and or not eq ne lt gt le ge cmp undef defined print say die warn"
);

const ML_KEYWORDS = words(
  "and as assert begin class constraint do done downto else end exception external false for fun "
  + "function functor if in include inherit initializer lazy let match method module mutable new nonrec "
  + "object of open or private rec sig struct then to true try type val virtual when while with "
  + "abstract base default delegate elif extern global inline interface internal member namespace null "
  + "override public return static upcast use void yield async"
);

const ELM_KEYWORDS = words("if then else case of let in type alias module exposing import as port");

const NIX_KEYWORDS = words("let in with rec inherit if then else assert import true false null or");

const HCL_KEYWORDS = words(
  "resource data variable output locals module provider terraform for_each count depends_on lifecycle "
  + "dynamic content for in if true false null"
);

const SOLIDITY_KEYWORDS = words(
  "pragma solidity contract interface library function modifier event struct enum mapping address bool "
  + "string bytes uint int uint8 uint256 int256 bytes32 public private internal external view pure "
  + "payable returns return if else for while do break continue emit require revert assert memory "
  + "storage calldata constructor fallback receive import is override virtual abstract new delete this "
  + "true false error unchecked immutable constant"
);

const VERILOG_KEYWORDS = words(
  "module endmodule input output inout wire reg logic bit always always_ff always_comb always_latch assign "
  + "begin end if else case casez casex endcase default for while parameter localparam integer genvar "
  + "generate endgenerate function endfunction task endtask posedge negedge initial typedef struct enum "
  + "package endpackage interface endinterface import modport"
);

const GLEAM_KEYWORDS = words(
  "as assert case const external fn if import let opaque panic pub todo try type use True False Nil"
);

const TCL_KEYWORDS = words(
  "proc set if else elseif for foreach while return puts expr switch global upvar incr list lindex "
  + "llength lappend string namespace package source catch error"
);

const LOG_LEVELS = upperWords("error err fatal critical crit panic warn warning info notice debug trace verbose");

const CSS_KEYWORDS = words(
  "@media @import @charset @keyframes @font-face @supports @namespace @page @layer @container @property "
  + "@tailwind @apply @use @forward @mixin @include @extend @if @else @each @function @return "
  + "!important from to inherit initial unset revert none auto"
);

const JSON_KEYWORDS = words("true false null");

const YAML_KEYWORDS = words("true false null yes no on off True False Null ~");

const GRAPHQL_KEYWORDS = words(
  "query mutation subscription fragment on type input enum interface union scalar schema directive "
  + "implements extend true false null message service rpc returns repeated optional required package "
  + "syntax import option reserved oneof map stream"
);

const NO_WORDS = new Set<string>();

const JS_STRINGS: readonly StringRule[] = [{ open: "`", close: "`", escape: true, multiline: true }, ...QUOTES];

const JAVASCRIPT: CodeGrammar = {
  ...CODE, ...SLASH_COMMENTS, keywords: JS_KEYWORDS, decorators: true, strings: JS_STRINGS
};

const C_GRAMMAR: CodeGrammar = { ...CODE, ...SLASH_COMMENTS, keywords: C_KEYWORDS, directive: "#", strings: QUOTES };

/**
 * Every grammar the viewer knows.
 *
 * Longer string openers must come before their prefixes: `'''` has to be tried
 * before `'`, or a Python docstring reads as an empty string followed by prose.
 */
const GRAMMARS: Record<string, CodeGrammar> = {
  typescript: JAVASCRIPT,
  javascript: JAVASCRIPT,
  tsx: { ...JAVASCRIPT, jsx: true },
  jsx: { ...JAVASCRIPT, jsx: true },
  // A Rust string literal may span lines; a char literal may not, but the two
  // are not told apart lexically, so both are treated as the former.
  rust: {
    ...CODE, ...SLASH_COMMENTS, keywords: RUST_KEYWORDS, macros: true, attributeOpeners: ["#![", "#["],
    strings: [
      { open: "\"", close: "\"", escape: true, multiline: true },
      { open: "'", close: "'", escape: true, multiline: false }
    ]
  },
  go: { ...CODE, ...SLASH_COMMENTS, keywords: GO_KEYWORDS, strings: [
    { open: "`", close: "`", escape: false, multiline: true }, ...QUOTES
  ] },
  java: { ...CODE, ...SLASH_COMMENTS, keywords: JAVA_KEYWORDS, decorators: true, strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true }, ...QUOTES
  ] },
  kotlin: { ...CODE, ...SLASH_COMMENTS, keywords: KOTLIN_KEYWORDS, decorators: true, strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: false, multiline: true }, ...QUOTES
  ] },
  scala: { ...CODE, ...SLASH_COMMENTS, keywords: SCALA_KEYWORDS, decorators: true, strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: false, multiline: true }, ...QUOTES
  ] },
  groovy: { ...CODE, ...SLASH_COMMENTS, keywords: GROOVY_KEYWORDS, decorators: true, strings: [
    ...TRIPLE_QUOTES, ...QUOTES
  ] },
  dart: { ...CODE, ...SLASH_COMMENTS, keywords: DART_KEYWORDS, decorators: true, strings: [
    ...TRIPLE_QUOTES, ...QUOTES
  ] },
  csharp: { ...CODE, ...SLASH_COMMENTS, keywords: CSHARP_KEYWORDS, directive: "#", strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: false, multiline: true }, ...QUOTES
  ] },
  swift: { ...CODE, ...SLASH_COMMENTS, keywords: SWIFT_KEYWORDS, decorators: true, directive: "#", strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true }, ...QUOTES
  ] },
  c: C_GRAMMAR,
  cpp: C_GRAMMAR,
  objectivec: { ...C_GRAMMAR, keywords: OBJC_KEYWORDS, decorators: true },
  shader: { ...C_GRAMMAR, keywords: SHADER_KEYWORDS },
  solidity: { ...CODE, ...SLASH_COMMENTS, keywords: SOLIDITY_KEYWORDS, strings: QUOTES },
  verilog: { ...CODE, ...SLASH_COMMENTS, keywords: VERILOG_KEYWORDS, directive: "`", strings: DOUBLE_QUOTE },
  zig: { ...CODE, ...SLASH_COMMENTS, keywords: ZIG_KEYWORDS, decorators: true, strings: QUOTES },
  php: {
    ...CODE,
    lineComments: ["//", "#"],
    blockComments: [["/*", "*/"]],
    keywords: PHP_KEYWORDS,
    sigils: "$",
    strings: QUOTES
  },
  python: {
    ...CODE,
    ...HASH_COMMENTS,
    keywords: PYTHON_KEYWORDS,
    decorators: true,
    strings: [...TRIPLE_QUOTES, ...QUOTES]
  },
  ruby: {
    ...CODE,
    lineComments: ["#"],
    blockComments: [["=begin", "=end"]],
    keywords: RUBY_KEYWORDS,
    wordExtra: "?!",
    sigils: "@",
    strings: QUOTES
  },
  perl: { ...CODE, ...HASH_COMMENTS, keywords: PERL_KEYWORDS, types: false, sigils: "$@%", strings: QUOTES },
  lua: {
    ...CODE,
    lineComments: ["--"],
    blockComments: [["--[[", "]]"]],
    keywords: LUA_KEYWORDS,
    strings: [{ open: "[[", close: "]]", escape: false, multiline: true }, ...QUOTES]
  },
  r: { ...CODE, ...HASH_COMMENTS, keywords: R_KEYWORDS, types: false, strings: QUOTES },
  julia: {
    ...CODE, ...HASH_COMMENTS, blockComments: [["#=", "=#"]], keywords: JULIA_KEYWORDS, decorators: true,
    strings: [{ open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true }, ...DOUBLE_QUOTE]
  },
  haskell: {
    ...CODE, lineComments: ["--"], blockComments: [["{-", "-}"]], keywords: HASKELL_KEYWORDS, calls: false,
    strings: DOUBLE_QUOTE
  },
  elm: {
    ...CODE, lineComments: ["--"], blockComments: [["{-", "-}"]], keywords: ELM_KEYWORDS, calls: false,
    strings: [{ open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true }, ...DOUBLE_QUOTE]
  },
  ocaml: {
    ...CODE, lineComments: ["//"], blockComments: [["(*", "*)"]], keywords: ML_KEYWORDS, calls: false,
    strings: DOUBLE_QUOTE
  },
  elixir: {
    ...CODE, ...HASH_COMMENTS, keywords: ELIXIR_KEYWORDS, decorators: true, wordExtra: "?!",
    strings: [...TRIPLE_QUOTES, ...QUOTES]
  },
  erlang: { ...CODE, lineComments: ["%"], blockComments: [], keywords: ERLANG_KEYWORDS, strings: DOUBLE_QUOTE },
  lisp: {
    ...CODE, lineComments: [";"], blockComments: [["#|", "|#"]], keywords: LISP_KEYWORDS, calls: false,
    types: false, wordExtra: "-!?*<>=/+", strings: DOUBLE_QUOTE
  },
  nim: {
    ...CODE, ...HASH_COMMENTS, blockComments: [["#[", "]#"]], keywords: NIM_KEYWORDS,
    strings: [{ open: "\"\"\"", close: "\"\"\"", escape: false, multiline: true }, ...QUOTES]
  },
  gleam: { ...CODE, ...SLASH_COMMENTS, keywords: GLEAM_KEYWORDS, decorators: true, strings: DOUBLE_QUOTE },
  tcl: { ...CODE, ...HASH_COMMENTS, keywords: TCL_KEYWORDS, types: false, calls: false, sigils: "$", strings: DOUBLE_QUOTE },
  shell: {
    ...DATA,
    ...HASH_COMMENTS,
    keywords: SHELL_KEYWORDS,
    sigils: "$",
    // A single-quoted shell string takes no escapes at all: `'\'` is a backslash.
    strings: [
      { open: "\"", close: "\"", escape: true, multiline: false },
      { open: "'", close: "'", escape: false, multiline: false }
    ]
  },
  makefile: {
    ...DATA, ...HASH_COMMENTS, keywords: words("ifeq ifneq ifdef ifndef else endif include define endef export override .PHONY"),
    sigils: "$", keys: "colon", wordExtra: ".", strings: QUOTES
  },
  dockerfile: {
    ...DATA, ...HASH_COMMENTS, keywords: DOCKER_KEYWORDS, caseInsensitive: true, sigils: "$", strings: QUOTES
  },
  cmake: {
    ...DATA, ...HASH_COMMENTS, keywords: CMAKE_KEYWORDS, caseInsensitive: true, sigils: "$", strings: DOUBLE_QUOTE
  },
  powershell: {
    ...CODE,
    lineComments: ["#"],
    blockComments: [["<#", "#>"]],
    keywords: POWERSHELL_KEYWORDS,
    caseInsensitive: true,
    types: false,
    // `Get-ChildItem` and `-eq` are single words there, which is why only this
    // grammar lets a hyphen into one.
    wordExtra: "-",
    sigils: "$",
    strings: [
      { open: "@\"", close: "\"@", escape: false, multiline: true },
      { open: "\"", close: "\"", escape: false, multiline: false },
      { open: "'", close: "'", escape: false, multiline: false }
    ]
  },
  batch: {
    ...DATA,
    lineComments: [],
    leadingComments: ["REM ", "REM\t", "::", "@REM "],
    blockComments: [],
    keywords: BATCH_KEYWORDS,
    caseInsensitive: true,
    sigils: "%",
    strings: [{ open: "\"", close: "\"", escape: false, multiline: false }]
  },
  vb: {
    ...CODE,
    lineComments: ["'"],
    leadingComments: ["REM "],
    blockComments: [],
    keywords: VB_KEYWORDS,
    caseInsensitive: true,
    types: false,
    directive: "#",
    strings: [{ open: "\"", close: "\"", escape: false, multiline: false }]
  },
  fortran: {
    ...CODE, lineComments: ["!"], blockComments: [], keywords: FORTRAN_KEYWORDS, caseInsensitive: true,
    types: false, strings: QUOTES
  },
  vhdl: {
    ...CODE, lineComments: ["--"], blockComments: [["/*", "*/"]], keywords: VHDL_KEYWORDS, caseInsensitive: true,
    types: false, strings: DOUBLE_QUOTE
  },
  nix: {
    ...DATA, lineComments: ["#"], blockComments: [["/*", "*/"]], keywords: NIX_KEYWORDS, keys: "equals",
    strings: [{ open: "''", close: "''", escape: false, multiline: true }, ...DOUBLE_QUOTE]
  },
  hcl: {
    ...DATA, lineComments: ["#", "//"], blockComments: [["/*", "*/"]], keywords: HCL_KEYWORDS, keys: "equals",
    calls: true, sigils: "$", strings: DOUBLE_QUOTE
  },
  latex: {
    ...DATA, lineComments: ["%"], blockComments: [], keywords: NO_WORDS, commandPrefix: "\\",
    strings: [{ open: "$$", close: "$$", escape: true, multiline: true }, { open: "$", close: "$", escape: true, multiline: false }]
  },
  assembly: {
    ...DATA, lineComments: [";", "#", "//"], blockComments: [["/*", "*/"]], keywords: NO_WORDS, keys: "colon",
    wordExtra: ".", strings: QUOTES
  },
  sql: {
    ...DATA,
    lineComments: ["--"],
    blockComments: [["/*", "*/"]],
    keywords: SQL_KEYWORDS,
    caseInsensitive: true,
    calls: true,
    strings: [
      { open: "'", close: "'", escape: false, multiline: false },
      { open: "\"", close: "\"", escape: false, multiline: false }
    ]
  },
  css: {
    ...DATA,
    lineComments: [],
    blockComments: [["/*", "*/"]],
    keywords: CSS_KEYWORDS,
    calls: true,
    wordExtra: "@-!",
    keys: "css",
    hexColors: true,
    strings: QUOTES
  },
  scss: {
    ...DATA,
    lineComments: ["//"],
    blockComments: [["/*", "*/"]],
    keywords: CSS_KEYWORDS,
    calls: true,
    wordExtra: "@-!",
    keys: "css",
    hexColors: true,
    sigils: "$",
    strings: QUOTES
  },
  json: { ...DATA, lineComments: ["//"], blockComments: [["/*", "*/"]], keywords: JSON_KEYWORDS, keys: "quoted", strings: QUOTES },
  yaml: { ...DATA, ...HASH_COMMENTS, keywords: YAML_KEYWORDS, keys: "colon", strings: QUOTES },
  toml: { ...DATA, lineComments: ["#"], blockComments: [], keywords: words("true false"), keys: "equals", sections: true, strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true },
    { open: "'''", close: "'''", escape: false, multiline: true },
    ...QUOTES
  ] },
  ini: {
    ...DATA, lineComments: ["#", ";"], blockComments: [], keywords: words("true false yes no on off"),
    keys: "equals", sections: true, strings: QUOTES
  },
  xml: { ...DATA, lineComments: [], blockComments: [["<!--", "-->"]], markup: true, keywords: NO_WORDS, strings: QUOTES },
  graphql: { ...CODE, lineComments: ["#", "//"], blockComments: [["/*", "*/"]], keywords: GRAPHQL_KEYWORDS, calls: false, decorators: true, strings: [
    { open: "\"\"\"", close: "\"\"\"", escape: true, multiline: true }, ...QUOTES
  ] },
  diff: { ...DATA, lineComments: [], blockComments: [], keywords: NO_WORDS, strings: [], lines: "diff" },
  markdown: {
    ...DATA,
    lineComments: [],
    blockComments: [["<!--", "-->"]],
    keywords: NO_WORDS,
    lines: "markdown",
    strings: [
      { open: "```", close: "```", escape: false, multiline: true },
      { open: "~~~", close: "~~~", escape: false, multiline: true },
      { open: "`", close: "`", escape: false, multiline: false }
    ]
  },
  log: { ...DATA, lineComments: [], blockComments: [], keywords: LOG_LEVELS, caseInsensitive: true, lines: "log", strings: DOUBLE_QUOTE }
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

/** `HashMap`, `Promise`, `T` — a capital followed by anything that is not all capitals. */
function isTypeName(word: string): boolean {
  return /^[A-Z]/.test(word) && (word.length === 1 || /[a-z]/.test(word));
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

/** Index just past the `]` that balances the `[` an attribute opener ends with. */
function findBracketClose(line: string, from: number): number {
  let depth = 1;
  for (let index = from; index < line.length; index += 1) {
    if (line[index] === "[") depth += 1;
    else if (line[index] === "]") {
      depth -= 1;
      if (depth === 0) return index + 1;
    }
  }
  return line.length;
}

/**
 * Where JSX may open a tag: after something that ends a statement or starts an
 * expression, never after an identifier or a closing bracket, which is where `<`
 * compares or opens a type argument list.
 */
function jsxMayOpen(line: string, index: number): boolean {
  let cursor = index - 1;
  while (cursor >= 0 && (line[cursor] === " " || line[cursor] === "\t")) cursor -= 1;
  if (cursor < 0) return true;
  if ("([{,;:=?!&|>".includes(line[cursor])) return true;
  return /\breturn$/.test(line.slice(0, cursor + 1));
}

/** Whole-line classification for the grammars where the first characters decide the line. */
function classifyLine(grammar: CodeGrammar, line: string): CodeToken[] | null {
  if (grammar.lines === "diff") {
    if (!line) return [];
    if (/^(?:diff |index |\+\+\+ |--- |new file mode|deleted file mode|similarity index|rename (?:from|to) |Binary files)/.test(line)) {
      return [{ kind: "meta", value: line }];
    }
    if (line.startsWith("@@")) {
      const close = line.indexOf("@@", 2);
      if (close < 0) return [{ kind: "meta", value: line }];
      const tokens: CodeToken[] = [{ kind: "meta", value: line.slice(0, close + 2) }];
      if (close + 2 < line.length) tokens.push({ kind: "plain", value: line.slice(close + 2) });
      return tokens;
    }
    if (line.startsWith("+")) return [{ kind: "inserted", value: line }];
    if (line.startsWith("-")) return [{ kind: "deleted", value: line }];
    return [{ kind: "plain", value: line }];
  }
  if (grammar.lines === "markdown") {
    if (/^ {0,3}#{1,6}(?:\s|$)/.test(line)) return [{ kind: "keyword", value: line }];
    if (/^ {0,3}>/.test(line)) return [{ kind: "comment", value: line }];
    if (/^ {0,3}(?:[-*_]\s*){3,}$/.test(line)) return [{ kind: "meta", value: line }];
  }
  return null;
}

/**
 * The key a line of configuration opens with, as `[indent, key, rest]`, or null.
 *
 * Only the start of a line is looked at: that is where a key is written, and a
 * colon or equals sign further along is a value's business.
 */
function leadingKey(grammar: CodeGrammar, line: string): [string, string] | null {
  let match: RegExpMatchArray | null = null;
  switch (grammar.keys) {
    case "colon":
      match = line.match(/^(\s*(?:-\s+)?)((?:"[^"]*"|'[^']*'|[^\s#:"'{}[\],&*!|>%@`][^#:]*?))(?=\s*:(?:\s|$))/);
      break;
    case "equals":
      match = line.match(/^(\s*)((?:"[^"]*"|'[^']*'|[A-Za-z0-9_.-]+(?:\s*\.\s*[A-Za-z0-9_-]+)*))(?=\s*=)/);
      break;
    case "css":
      // A declaration, not a selector: nothing on the line opens a block.
      if (line.includes("{")) return null;
      match = line.match(/^(\s*)(--[A-Za-z0-9_-]+|-?[a-z][a-z0-9-]*)(?=\s*:)/);
      break;
    default:
      return null;
  }
  return match ? [match[1], match[2]] : null;
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
  let inTag = false;
  // Braces opened inside a JSX tag: `onClick={() => x}` holds a `>` that does not
  // close the tag, and words in there are code rather than attribute names.
  let tagBraces = 0;

  const flushPlain = (end: number) => {
    if (end > plainFrom) tokens.push({ kind: "plain", value: line.slice(plainFrom, end) });
  };
  const push = (kind: CodeTokenKind, from: number, to: number) => {
    flushPlain(from);
    tokens.push({ kind, value: line.slice(from, to) });
    plainFrom = to;
  };

  if (block?.kind === "tag") {
    inTag = true;
  } else if (block) {
    const end = findClose(line, 0, block.close, block.escape);
    if (end < 0) {
      return { tokens: line ? [{ kind: block.kind, value: line }] : [], block };
    }
    tokens.push({ kind: block.kind, value: line.slice(0, end) });
    index = end;
    plainFrom = end;
  } else {
    const whole = classifyLine(grammar, line);
    if (whole) return { tokens: whole, block: null };

    const indent = line.length - line.trimStart().length;
    const leading = line.slice(indent);
    const marker = grammar.leadingComments?.find((prefix) => (
      leading.toUpperCase().startsWith(prefix) || leading.toUpperCase() === prefix.trim()
    ));
    if (marker !== undefined) {
      if (indent) tokens.push({ kind: "plain", value: line.slice(0, indent) });
      tokens.push({ kind: "comment", value: leading });
      return { tokens, block: null };
    }

    const section = grammar.sections ? line.match(/^\s*(\[\[?[^\]]*\]\]?)\s*(?:[#;].*)?$/) : null;
    if (section) {
      const end = indent + section[1].length;
      push("type", indent, end);
      index = end;
    } else if (grammar.directive && leading.startsWith(grammar.directive)) {
      const match = leading.match(/^.\s*[A-Za-z_]\w*/);
      if (match) {
        push("meta", indent, indent + match[0].length);
        index = indent + match[0].length;
        // `#include <stdio.h>`: the bracketed name is the directive's argument.
        const argument = line.slice(index).match(/^\s*<[^>]*>/);
        if (argument) {
          const start = index + argument[0].indexOf("<");
          push("string", start, index + argument[0].length);
          index += argument[0].length;
        }
      }
    } else if (grammar.lines === "markdown") {
      const list = line.match(/^(\s*)([-*+]|\d{1,9}[.)])(?=\s)/);
      if (list) {
        push("meta", list[1].length, list[0].length);
        index = list[0].length;
      }
    } else {
      const key = leadingKey(grammar, line);
      if (key) {
        push("property", key[0].length, key[0].length + key[1].length);
        index = key[0].length + key[1].length;
      }
    }
  }

  while (index < line.length) {
    const character = line[index];

    if (inTag && grammar.jsx && (character === "{" || character === "}")) {
      tagBraces = Math.max(0, tagBraces + (character === "{" ? 1 : -1));
    }
    if (inTag && tagBraces === 0 && (character === ">" || line.startsWith("/>", index))) {
      const end = index + (character === ">" ? 1 : 2);
      push("tag", index, end);
      index = end;
      inTag = false;
      continue;
    }

    const lineComment = grammar.lineComments.find((marker) => line.startsWith(marker, index));
    // A `#` in the middle of a word — `C#`, a URL fragment — is not a comment in a
    // language whose comments start with one.
    if (lineComment !== undefined && !(lineComment === "#" && index > 0 && isWordCharacter(grammar, line[index - 1]) && grammar.sigils?.includes("$"))) {
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

    const attributeOpener = grammar.attributeOpeners?.find((opener) => line.startsWith(opener, index));
    if (attributeOpener !== undefined) {
      const end = findBracketClose(line, index + attributeOpener.length);
      push("meta", index, end);
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
      // `"key": value` — the string before a colon is the key it names.
      const key = grammar.keys === "quoted" && /^\s*:/.test(line.slice(end));
      push(key ? "property" : "string", index, end);
      index = end;
      continue;
    }

    // A closing `</Tag` has no other reading in code, so it needs no context.
    const jsxTag = grammar.jsx && (jsxMayOpen(line, index) || /^<\/[A-Za-z]/.test(line.slice(index, index + 3)));
    if ((grammar.markup || jsxTag) && character === "<") {
      let cursor = index + 1;
      if (line[cursor] === "/") cursor += 1;
      const start = cursor;
      while (cursor < line.length && /[A-Za-z0-9_:.!?-]/.test(line[cursor])) cursor += 1;
      // `<>` and `</>` are JSX fragments; everything else needs a name.
      const fragment = grammar.jsx && line[cursor] === ">" && cursor === start;
      if ((cursor > start && /[A-Za-z!?]/.test(line[start])) || fragment) {
        push("tag", index, cursor);
        index = cursor;
        inTag = !fragment;
        tagBraces = 0;
        continue;
      }
    }

    if (grammar.sigils?.includes(character)) {
      let cursor = index + 1;
      if (line[cursor] === "{" || (line[cursor] === "(" && grammar.keys === "colon")) {
        const close = line[cursor] === "{" ? "}" : ")";
        const end = line.indexOf(close, cursor);
        cursor = end < 0 ? line.length : end + 1;
      } else if (/[A-Za-z_]/.test(line[cursor] ?? "")) {
        while (cursor < line.length && /[A-Za-z0-9_]/.test(line[cursor])) cursor += 1;
        // `%PATH%` closes itself.
        if (character === "%" && line[cursor] === "%") cursor += 1;
      } else if (/[0-9@*#?$!-]/.test(line[cursor] ?? "") && character !== "%") {
        cursor += 1;
      } else if (character === "%" && line[cursor] === "%" && /[A-Za-z]/.test(line[cursor + 1] ?? "")) {
        cursor += 2;
      } else if (character === "%" && /[0-9*]/.test(line[cursor] ?? "")) {
        cursor += 1;
      }
      if (cursor > index + 1) {
        push("variable", index, cursor);
        index = cursor;
        continue;
      }
    }

    if (grammar.commandPrefix && line.startsWith(grammar.commandPrefix, index)) {
      let cursor = index + grammar.commandPrefix.length;
      if (/[A-Za-z@]/.test(line[cursor] ?? "")) {
        while (cursor < line.length && /[A-Za-z@]/.test(line[cursor])) cursor += 1;
      } else if (cursor < line.length) {
        cursor += 1;
      }
      push("keyword", index, cursor);
      index = cursor;
      continue;
    }

    if (grammar.decorators && character === "@" && /[A-Za-z_]/.test(line[index + 1] ?? "")) {
      let cursor = index + 1;
      while (cursor < line.length && /[A-Za-z0-9_.]/.test(line[cursor])) cursor += 1;
      push("meta", index, cursor);
      index = cursor;
      continue;
    }

    if (grammar.hexColors && character === "#" && /^#[0-9A-Fa-f]{3,8}\b/.test(line.slice(index))) {
      const match = line.slice(index).match(/^#[0-9A-Fa-f]{3,8}\b/)!;
      push("number", index, index + match[0].length);
      index += match[0].length;
      continue;
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
      const next = line[cursor];
      // Case folding is the rule of a few grammars — SQL, BASIC, batch — and no
      // one else's: `CONST` is not a TypeScript keyword because `const` is.
      const reserved = grammar.caseInsensitive
        ? grammar.keywords.has(word.toUpperCase())
        : grammar.keywords.has(word);
      if (inTag && tagBraces === 0 && (next === "=" || /^\s*=(?!=)/.test(line.slice(cursor)))) {
        push("attribute", index, cursor);
      } else if (reserved) {
        push("keyword", index, cursor);
      } else if (grammar.macros && next === "!" && /[([{]/.test(line[cursor + 1] ?? "")) {
        push("function", index, cursor + 1);
        cursor += 1;
      } else if (grammar.calls && next === "(") {
        push("function", index, cursor);
      } else if (grammar.types && isTypeName(word)) {
        push("type", index, cursor);
      } else if (inTag && tagBraces === 0 && grammar.jsx && index > 0 && /\s/.test(line[index - 1])) {
        // A bare attribute in JSX — `<input disabled />` — has no `=` to go by.
        push("attribute", index, cursor);
      }
      index = cursor;
      continue;
    }

    index += 1;
  }

  flushPlain(line.length);
  return { tokens, block: inTag ? { kind: "tag", close: ">", escape: false } : null };
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
  return lines.map((line, index) => {
    // `#!/usr/bin/env node` says how to run the file, whatever the file is in.
    if (index === 0 && line.startsWith("#!") && block === null) return [{ kind: "meta" as const, value: line }];
    const result = highlightCodeLine(grammar, line, block);
    block = result.block;
    return result.tokens;
  });
}
