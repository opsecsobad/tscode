//! Code completion: VS Code's suggest widget, without language servers.
//!
//! Suggestions come from two places:
//!
//! * per-language tables: keywords, built-in functions / types / constants, common
//!   modules, common methods (offered after `.`, `->`, `::`...) and snippets;
//! * the words in the open documents of the same language (VS Code's word-based
//!   suggestions), with a little guessing about what each one is: a name after
//!   `def` / `fn` / `function` is a function, after `class` / `struct` a class, a
//!   name followed by `(` is called, ALL_CAPS is a constant...
//!
//! HTML and CSS get context of their own (tags, attributes, properties, values), and
//! XML suggests the tag names already used in the file.
//!
//! `candidates` builds the list once when the widget opens; `Suggest::filter`
//! narrows it on every key press with the same fuzzy matcher as Quick Open.

use crate::editor::Doc;
use crate::icons::ascii;
use crate::screen::{ellipsize, width, Buffer, Rect, Style, BOLD};
use crate::theme::{capture_names, VSTheme};
use crate::util::fuzzy;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Rows the widget shows before it scrolls (VS Code shows 12).
const VISIBLE: usize = 12;
/// Lines either side of the cursor scanned for words in the active document.
const SCAN_LINES: usize = 10_000;
const MAX_WORDS: usize = 20_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Snippet,
    Keyword,
    Function,
    Method,
    Class,
    Module,
    Variable,
    Field,
    Property,
    Constant,
    Value,
    Text,
}

impl Kind {
    /// The codicon VS Code uses for this kind (or a letter without a Nerd Font).
    pub fn glyph(self) -> &'static str {
        if ascii() {
            return match self {
                Kind::Snippet => "s",
                Kind::Keyword => "k",
                Kind::Function | Kind::Method => "f",
                Kind::Class => "c",
                Kind::Module => "m",
                Kind::Variable | Kind::Field => "v",
                Kind::Property => "p",
                Kind::Constant => "C",
                Kind::Value => "e",
                Kind::Text => "t",
            };
        }
        match self {
            Kind::Snippet => "\u{eb66}",
            Kind::Keyword => "\u{eb62}",
            Kind::Function | Kind::Method => "\u{ea8c}",
            Kind::Class => "\u{eb5b}",
            Kind::Module => "\u{ea8b}",
            Kind::Variable => "\u{ea88}",
            Kind::Field => "\u{eb5f}",
            Kind::Property => "\u{eb65}",
            Kind::Constant => "\u{eb5d}",
            Kind::Value => "\u{eb5e}",
            Kind::Text => "\u{ea93}",
        }
    }

    /// The theme key for the icon's colour.
    pub fn colour_key(self) -> &'static str {
        match self {
            Kind::Snippet => "symbolIcon.snippetForeground",
            Kind::Keyword => "symbolIcon.keywordForeground",
            Kind::Function => "symbolIcon.functionForeground",
            Kind::Method => "symbolIcon.methodForeground",
            Kind::Class => "symbolIcon.classForeground",
            Kind::Module => "symbolIcon.moduleForeground",
            Kind::Variable => "symbolIcon.variableForeground",
            Kind::Field => "symbolIcon.fieldForeground",
            Kind::Property => "symbolIcon.propertyForeground",
            Kind::Constant => "symbolIcon.constantForeground",
            Kind::Value => "symbolIcon.enumeratorMemberForeground",
            Kind::Text => "symbolIcon.textForeground",
        }
    }
}

/// One suggestion.
#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    pub kind: Kind,
    /// Shown on the right of the selected row.
    pub detail: String,
    /// Snippet text (`$1`, `${1:default}`, `$0`) to insert instead of the label.
    pub body: Option<String>,
    /// From a language table (SQL keywords follow the case you type); words from the
    /// document are inserted exactly as written.
    pub builtin: bool,
    /// Order of the list it came from; with nothing typed yet, earlier groups come first.
    group: u8,
}

impl Item {
    fn new(label: &str, kind: Kind, detail: &str, body: Option<&str>) -> Item {
        Item { label: label.to_string(), kind, detail: detail.to_string(), body: body.map(str::to_string), builtin: true, group: 0 }
    }

    fn word(label: String, kind: Kind, body: Option<String>) -> Item {
        Item { label, kind, detail: String::new(), body, builtin: false, group: 0 }
    }
}

// -- language tables ----------------------------------------------------------------------

const S: Kind = Kind::Snippet;
const F: Kind = Kind::Function;

type Snippet = (Kind, &'static str, &'static str, &'static str);

struct Lang {
    ids: &'static [&'static str],
    /// Characters that belong to a word besides letters, digits and `_`.
    extra: &'static str,
    /// Text right before the cursor that opens the widget by itself (`.`, `->`...).
    triggers: &'static [&'static str],
    /// Of those, the ones after which methods / fields are offered.
    members_after: &'static [&'static str],
    /// Open the widget while typing (VS Code leaves it off for Markdown).
    auto: bool,
    /// Suggest inside strings too (data and markup languages).
    in_strings: bool,
    /// Keywords match and are inserted in the case you type (SQL).
    ignore_case: bool,
    keywords: &'static str,
    functions: &'static str,
    types: &'static str,
    constants: &'static str,
    modules: &'static str,
    variables: &'static str,
    members: &'static str,
    /// Offered after `::` (associated functions, namespaces).
    paths: &'static str,
    /// Line comment markers.
    comments: &'static [&'static str],
    snippets: &'static [&'static [Snippet]],
}

const EMPTY: Lang = Lang {
    ids: &[],
    extra: "",
    triggers: &[],
    members_after: &[],
    auto: true,
    in_strings: false,
    ignore_case: false,
    keywords: "",
    functions: "",
    types: "",
    constants: "",
    modules: "",
    variables: "",
    members: "",
    paths: "",
    comments: &[],
    snippets: &[],
};

const PYTHON_SNIPPETS: &[Snippet] = &[
    (S, "def", "def ${1:name}($2):\n\t${0:pass}", "Function definition"),
    (S, "adef", "async def ${1:name}($2):\n\t${0:pass}", "Async function definition"),
    (S, "class", "class ${1:Name}:\n\tdef __init__(self$2):\n\t\t${0:pass}", "Class definition"),
    (S, "if", "if ${1:condition}:\n\t${0:pass}", "if statement"),
    (S, "ifelse", "if ${1:condition}:\n\t${2:pass}\nelse:\n\t${0:pass}", "if/else statement"),
    (S, "for", "for ${1:item} in ${2:items}:\n\t${0:pass}", "for loop"),
    (S, "while", "while ${1:condition}:\n\t${0:pass}", "while loop"),
    (S, "try", "try:\n\t${1:pass}\nexcept ${2:Exception} as ${3:e}:\n\t${0:raise}", "try/except"),
    (S, "with", "with ${1:expression} as ${2:target}:\n\t${0:pass}", "with statement"),
    (S, "ifmain", "if __name__ == \"__main__\":\n\t${0:main()}", "Main guard"),
    (S, "lambda", "lambda ${1:x}: ${0:x}", "Lambda expression"),
];

const RUST_SNIPPETS: &[Snippet] = &[
    (S, "fn", "fn ${1:name}($2) {\n\t$0\n}", "Function"),
    (S, "pfn", "pub fn ${1:name}($2) {\n\t$0\n}", "Public function"),
    (S, "struct", "struct ${1:Name} {\n\t$0\n}", "Struct"),
    (S, "enum", "enum ${1:Name} {\n\t$0\n}", "Enum"),
    (S, "impl", "impl ${1:Type} {\n\t$0\n}", "Impl block"),
    (S, "trait", "trait ${1:Name} {\n\t$0\n}", "Trait"),
    (S, "match", "match ${1:value} {\n\t${2:pattern} => $0,\n}", "Match expression"),
    (S, "if", "if ${1:condition} {\n\t$0\n}", "if expression"),
    (S, "iflet", "if let Some(${1:value}) = ${2:option} {\n\t$0\n}", "if let"),
    (S, "for", "for ${1:item} in ${2:iter} {\n\t$0\n}", "for loop"),
    (S, "while", "while ${1:condition} {\n\t$0\n}", "while loop"),
    (S, "loop", "loop {\n\t$0\n}", "loop"),
    (S, "main", "fn main() {\n\t$0\n}", "main function"),
    (S, "test", "#[test]\nfn ${1:name}() {\n\t$0\n}", "Test function"),
    (S, "derive", "#[derive(${1:Debug, Clone})]", "derive attribute"),
    (F, "println!", "println!(\"$1\"$0);", "macro"),
    (F, "print!", "print!(\"$1\"$0);", "macro"),
    (F, "eprintln!", "eprintln!(\"$1\"$0);", "macro"),
    (F, "format!", "format!(\"$1\"$0)", "macro"),
    (F, "write!", "write!(${1:f}, \"$2\"$0)", "macro"),
    (F, "writeln!", "writeln!(${1:f}, \"$2\"$0)", "macro"),
    (F, "vec!", "vec![$0]", "macro"),
    (F, "panic!", "panic!(\"$0\")", "macro"),
    (F, "assert!", "assert!($0);", "macro"),
    (F, "assert_eq!", "assert_eq!($1, $0);", "macro"),
    (F, "assert_ne!", "assert_ne!($1, $0);", "macro"),
    (F, "dbg!", "dbg!($0)", "macro"),
    (F, "todo!", "todo!()", "macro"),
    (F, "unimplemented!", "unimplemented!()", "macro"),
    (F, "unreachable!", "unreachable!()", "macro"),
    (F, "matches!", "matches!($1, $0)", "macro"),
];

const JS_SNIPPETS: &[Snippet] = &[
    (S, "log", "console.log($0);", "Log to the console"),
    (S, "function", "function ${1:name}(${2:params}) {\n\t$0\n}", "Function Statement"),
    (S, "arrow", "(${1:params}) => {\n\t$0\n}", "Arrow Function"),
    (S, "if", "if (${1:condition}) {\n\t$0\n}", "If Statement"),
    (S, "ifelse", "if (${1:condition}) {\n\t$2\n} else {\n\t$0\n}", "If-Else Statement"),
    (S, "for", "for (let ${1:index} = 0; $1 < ${2:array}.length; $1++) {\n\t$0\n}", "For Loop"),
    (S, "forof", "for (const ${1:item} of ${2:items}) {\n\t$0\n}", "For-Of Loop"),
    (S, "forin", "for (const ${1:key} in ${2:object}) {\n\t$0\n}", "For-In Loop"),
    (S, "foreach", "${1:array}.forEach((${2:element}) => {\n\t$0\n});", "For-Each Loop"),
    (S, "while", "while (${1:condition}) {\n\t$0\n}", "While Statement"),
    (S, "switch", "switch (${1:key}) {\n\tcase ${2:value}:\n\t\t$0\n\t\tbreak;\n\n\tdefault:\n\t\tbreak;\n}", "Switch Statement"),
    (S, "trycatch", "try {\n\t$1\n} catch (${2:error}) {\n\t$0\n}", "Try-Catch Statement"),
    (S, "class", "class ${1:Name} {\n\tconstructor(${2:parameters}) {\n\t\t$0\n\t}\n}", "Class Definition"),
    (S, "import", "import { $2 } from \"${1:module}\";", "Import external module"),
    (S, "settimeout", "setTimeout(() => {\n\t$0\n}, ${1:timeout});", "Set Timeout Function"),
    (S, "new", "const ${1:name} = new ${2:Type}($3);$0", "New Statement"),
];

const TS_SNIPPETS: &[Snippet] = &[
    (S, "interface", "interface ${1:Name} {\n\t$0\n}", "Interface Declaration"),
    (S, "type", "type ${1:Name} = $0;", "Type Alias"),
    (S, "enum", "enum ${1:Name} {\n\t$0\n}", "Enum Declaration"),
];

const JAVA_SNIPPETS: &[Snippet] = &[
    (S, "main", "public static void main(String[] args) {\n\t$0\n}", "main method"),
    (S, "sysout", "System.out.println($0);", "Print to standard out"),
    (S, "syserr", "System.err.println($0);", "Print to standard err"),
    (S, "class", "public class ${1:Name} {\n\t$0\n}", "Public class"),
    (S, "interface", "public interface ${1:Name} {\n\t$0\n}", "Public interface"),
    (S, "method", "${1:public} ${2:void} ${3:name}($4) {\n\t$0\n}", "Method"),
    (S, "ctor", "public ${1:Name}($2) {\n\t$0\n}", "Constructor"),
    (S, "if", "if (${1:condition}) {\n\t$0\n}", "if statement"),
    (S, "for", "for (int ${1:i} = 0; $1 < ${2:n}; $1++) {\n\t$0\n}", "for loop"),
    (S, "foreach", "for (${1:var} ${2:item} : ${3:items}) {\n\t$0\n}", "Enhanced for loop"),
    (S, "while", "while (${1:condition}) {\n\t$0\n}", "while loop"),
    (S, "try", "try {\n\t$1\n} catch (${2:Exception} e) {\n\t$0\n}", "try/catch"),
];

const C_COMMON_SNIPPETS: &[Snippet] = &[
    (S, "#include", "#include <${1:stdio.h}>", "Include a header"),
    (S, "#ifndef", "#ifndef ${1:HEADER_H}\n#define $1\n\n$0\n\n#endif", "Include guard"),
    (S, "if", "if (${1:condition}) {\n\t$0\n}", "if statement"),
    (S, "else", "else {\n\t$0\n}", "else block"),
    (S, "for", "for (int ${1:i} = 0; $1 < ${2:n}; $1++) {\n\t$0\n}", "for loop"),
    (S, "while", "while (${1:condition}) {\n\t$0\n}", "while loop"),
    (S, "do", "do {\n\t$0\n} while (${1:condition});", "do-while loop"),
    (S, "switch", "switch (${1:value}) {\ncase ${2:x}:\n\t$0\n\tbreak;\ndefault:\n\tbreak;\n}", "switch statement"),
    (S, "struct", "struct ${1:name} {\n\t$0\n};", "struct"),
    (S, "typedef", "typedef struct ${1:name} {\n\t$0\n} ${2:Name};", "typedef struct"),
    (S, "printf", "printf(\"${1:%s}\\n\"$0);", "printf"),
];

const C_SNIPPETS: &[Snippet] = &[(S, "main", "int main(int argc, char *argv[]) {\n\t$0\n\treturn 0;\n}", "main function")];

const CPP_SNIPPETS: &[Snippet] = &[
    (S, "main", "int main() {\n\t$0\n\treturn 0;\n}", "main function"),
    (S, "class", "class ${1:Name} {\npublic:\n\t$1();\n\t~$1();\n\nprivate:\n\t$0\n};", "class"),
    (S, "namespace", "namespace ${1:name} {\n\n$0\n\n}", "namespace"),
    (S, "forr", "for (auto& ${1:item} : ${2:items}) {\n\t$0\n}", "range-based for loop"),
    (S, "cout", "std::cout << $1 << std::endl;", "Print to standard out"),
    (S, "try", "try {\n\t$1\n} catch (const ${2:std::exception}& e) {\n\t$0\n}", "try/catch"),
];

const GO_SNIPPETS: &[Snippet] = &[
    (S, "func", "func ${1:name}($2) {\n\t$0\n}", "Function declaration"),
    (S, "meth", "func (${1:r} ${2:Type}) ${3:name}($4) {\n\t$0\n}", "Method declaration"),
    (S, "main", "func main() {\n\t$0\n}", "main function"),
    (S, "pkgm", "package main\n\nfunc main() {\n\t$0\n}", "main package"),
    (S, "if", "if ${1:condition} {\n\t$0\n}", "if statement"),
    (S, "iferr", "if err != nil {\n\treturn ${1:err}\n}", "if err != nil"),
    (S, "for", "for ${1:i} := 0; $1 < ${2:n}; $1++ {\n\t$0\n}", "for loop"),
    (S, "forr", "for ${1:_}, ${2:v} := range ${3:items} {\n\t$0\n}", "for range"),
    (S, "struct", "type ${1:Name} struct {\n\t$0\n}", "struct type"),
    (S, "interface", "type ${1:Name} interface {\n\t$0\n}", "interface type"),
    (S, "switch", "switch ${1:value} {\ncase ${2:x}:\n\t$0\n}", "switch statement"),
    (S, "go", "go func() {\n\t$0\n}()", "goroutine"),
    (S, "fp", "fmt.Println($0)", "fmt.Println"),
    (S, "ff", "fmt.Printf(\"$1\\n\", $0)", "fmt.Printf"),
];

const BASH_SNIPPETS: &[Snippet] = &[
    (S, "if", "if [[ ${1:condition} ]]; then\n\t$0\nfi", "if statement"),
    (S, "ifelse", "if [[ ${1:condition} ]]; then\n\t$2\nelse\n\t$0\nfi", "if/else statement"),
    (S, "for", "for ${1:item} in ${2:items}; do\n\t$0\ndone", "for loop"),
    (S, "while", "while ${1:condition}; do\n\t$0\ndone", "while loop"),
    (S, "case", "case ${1:word} in\n\t${2:pattern})\n\t\t$0\n\t\t;;\nesac", "case statement"),
    (S, "function", "${1:name}() {\n\t$0\n}", "function"),
    (S, "shebang", "#!/usr/bin/env bash\n$0", "Shebang line"),
    (S, "strict", "set -euo pipefail\n$0", "Strict mode"),
];

const SQL_SNIPPETS: &[Snippet] = &[
    (S, "select-from", "SELECT ${1:*}\nFROM ${2:table}\nWHERE ${3:condition};", "SELECT statement"),
    (S, "insert-into", "INSERT INTO ${1:table} (${2:columns})\nVALUES (${3:values});", "INSERT statement"),
    (S, "update-set", "UPDATE ${1:table}\nSET ${2:column} = ${3:value}\nWHERE ${4:condition};", "UPDATE statement"),
    (S, "delete-from", "DELETE FROM ${1:table}\nWHERE ${2:condition};", "DELETE statement"),
    (S, "create-table", "CREATE TABLE ${1:name} (\n\t${2:id} INTEGER PRIMARY KEY,\n\t$0\n);", "CREATE TABLE statement"),
];

const RUBY_SNIPPETS: &[Snippet] = &[
    (S, "def", "def ${1:name}$2\n\t$0\nend", "Method definition"),
    (S, "class", "class ${1:Name}\n\tdef initialize$2\n\t\t$0\n\tend\nend", "Class definition"),
    (S, "module", "module ${1:Name}\n\t$0\nend", "Module definition"),
    (S, "if", "if ${1:condition}\n\t$0\nend", "if statement"),
    (S, "unless", "unless ${1:condition}\n\t$0\nend", "unless statement"),
    (S, "each", "each do |${1:item}|\n\t$0\nend", "each block"),
    (S, "do", "do |${1:args}|\n\t$0\nend", "do block"),
    (S, "case", "case ${1:value}\nwhen ${2:x}\n\t$0\nend", "case statement"),
    (S, "begin", "begin\n\t$1\nrescue ${2:StandardError} => e\n\t$0\nend", "begin/rescue"),
];

const LUA_SNIPPETS: &[Snippet] = &[
    (S, "function", "function ${1:name}($2)\n\t$0\nend", "function"),
    (S, "lfunction", "local function ${1:name}($2)\n\t$0\nend", "local function"),
    (S, "if", "if ${1:condition} then\n\t$0\nend", "if statement"),
    (S, "for", "for ${1:i} = ${2:1}, ${3:n} do\n\t$0\nend", "numeric for loop"),
    (S, "fori", "for ${1:i}, ${2:v} in ipairs(${3:t}) do\n\t$0\nend", "for ipairs"),
    (S, "forp", "for ${1:k}, ${2:v} in pairs(${3:t}) do\n\t$0\nend", "for pairs"),
    (S, "while", "while ${1:condition} do\n\t$0\nend", "while loop"),
    (S, "repeat", "repeat\n\t$0\nuntil ${1:condition}", "repeat loop"),
    (S, "req", "local ${1:mod} = require(\"${2:mod}\")", "require a module"),
];

const PHP_SNIPPETS: &[Snippet] = &[
    (S, "php", "<?php\n\n$0", "PHP opening tag"),
    (S, "function", "function ${1:name}($2)\n{\n\t$0\n}", "function"),
    (S, "class", "class ${1:Name}\n{\n\tpublic function __construct($2)\n\t{\n\t\t$0\n\t}\n}", "class"),
    (S, "if", "if (${1:condition}) {\n\t$0\n}", "if statement"),
    (S, "foreach", "foreach (${1:\\$items} as ${2:\\$item}) {\n\t$0\n}", "foreach loop"),
    (S, "for", "for (${1:\\$i} = 0; $1 < ${2:\\$n}; $1++) {\n\t$0\n}", "for loop"),
    (S, "while", "while (${1:condition}) {\n\t$0\n}", "while loop"),
    (S, "try", "try {\n\t$1\n} catch (${2:Exception} \\$e) {\n\t$0\n}", "try/catch"),
];

const HTML_SNIPPETS: &[Snippet] = &[(
    S,
    "html5",
    "<!DOCTYPE html>\n<html lang=\"${1:en}\">\n<head>\n\t<meta charset=\"UTF-8\">\n\t<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n\t<title>${2:Document}</title>\n</head>\n<body>\n\t$0\n</body>\n</html>",
    "HTML5 document",
)];

const MARKDOWN_SNIPPETS: &[Snippet] = &[
    (S, "codeblock", "```${1:language}\n$0\n```", "Fenced code block"),
    (S, "link", "[${1:text}](${2:url})", "Link"),
    (S, "image", "![${1:alt}](${2:url})", "Image"),
    (S, "table", "| ${1:Column} | ${2:Column} |\n| --- | --- |\n| $3 | $0 |", "Table"),
];

macro_rules! js_keywords {
    () => {
        "break case catch class const continue debugger default delete do else export extends false finally for function if import in instanceof let new null return super switch this throw true try typeof undefined var void while with yield async await of static get set"
    };
}
macro_rules! js_globals {
    () => {
        "Array Object String Number Boolean Promise Date RegExp Map Set WeakMap WeakSet Error TypeError Symbol BigInt Math JSON Intl Reflect Proxy"
    };
}
macro_rules! js_functions {
    () => {
        "parseInt parseFloat isNaN isFinite setTimeout setInterval clearTimeout clearInterval fetch require structuredClone queueMicrotask encodeURIComponent decodeURIComponent"
    };
}
macro_rules! js_members {
    () => {
        "log error warn info debug table length push pop shift unshift slice splice concat join reverse sort map filter reduce forEach find findIndex some every includes indexOf flat flatMap fill keys values entries then catch finally toString toFixed split trim trimStart trimEnd replace replaceAll startsWith endsWith toLowerCase toUpperCase padStart padEnd charAt substring match test bind call apply addEventListener removeEventListener querySelector querySelectorAll getElementById createElement appendChild remove classList textContent innerHTML style setAttribute getAttribute preventDefault stopPropagation json text stringify parse assign freeze floor ceil round random max min abs"
    };
}
macro_rules! c_keywords {
    () => {
        "auto break case char const continue default do double else enum extern float for goto if inline int long register restrict return short signed sizeof static struct switch typedef union unsigned void volatile while bool true false #define #ifdef #ifndef #endif #if #elif #else #pragma #undef"
    };
}
macro_rules! c_functions {
    () => {
        "printf fprintf sprintf snprintf scanf sscanf puts putchar getchar fgets fputs fopen fclose fread fwrite fflush malloc calloc realloc free memcpy memmove memset memcmp strlen strcpy strncpy strcat strcmp strncmp strchr strrchr strstr strdup strtok atoi atof strtol strtoul exit abort assert qsort bsearch rand srand time perror"
    };
}
macro_rules! c_types {
    () => {
        "size_t ssize_t FILE int8_t int16_t int32_t int64_t uint8_t uint16_t uint32_t uint64_t uintptr_t intptr_t ptrdiff_t"
    };
}

const LANGS: &[Lang] = &[
    Lang {
        ids: &["python"],
        triggers: &["."],
        members_after: &["."],
        keywords: "False None True and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield match case",
        functions: "abs aiter all anext any ascii bin breakpoint callable chr compile delattr dir divmod enumerate eval exec filter format getattr globals hasattr hash help hex id input isinstance issubclass iter len locals map max min next oct open ord pow print repr reversed round setattr sorted sum super vars zip __import__",
        types: "bool bytearray bytes complex dict float frozenset int list memoryview object property range set slice staticmethod classmethod str tuple type Exception BaseException ValueError TypeError KeyError IndexError AttributeError RuntimeError NotImplementedError StopIteration OSError FileNotFoundError ImportError ZeroDivisionError AssertionError KeyboardInterrupt PermissionError TimeoutError",
        constants: "NotImplemented Ellipsis __name__ __file__ __doc__",
        variables: "self cls",
        modules: "os sys re json math time datetime pathlib subprocess collections itertools functools typing dataclasses asyncio logging random shutil argparse unittest",
        members: "append extend insert pop remove clear copy count index sort reverse keys values items get update setdefault join split rsplit strip lstrip rstrip replace format startswith endswith lower upper title find encode decode splitlines read write readlines close add discard union intersection difference",
        comments: &["#"],
        snippets: &[PYTHON_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["rust"],
        triggers: &[".", "::"],
        members_after: &[".", "::"],
        keywords: "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while",
        types: "i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 bool char str String Vec Option Result Box Rc Arc RefCell Cell HashMap HashSet BTreeMap BTreeSet VecDeque Path PathBuf Mutex",
        constants: "Some None Ok Err",
        modules: "std core alloc",
        members: "unwrap expect unwrap_or unwrap_or_default unwrap_or_else map map_err and_then ok ok_or is_some is_none is_ok is_err as_ref as_mut as_str as_bytes clone to_string to_owned into iter iter_mut into_iter collect filter filter_map for_each enumerate zip rev take skip chain fold any all find position count sum max min len is_empty push push_str pop insert remove get get_mut contains extend sort sort_by dedup retain join split trim lines chars bytes starts_with ends_with replace parse borrow borrow_mut lock read write",
        paths: "new default from with_capacity from_str from_utf8 into_iter iter collections fmt fs io env path process sync thread time",
        comments: &["//"],
        snippets: &[RUST_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["javascript"],
        extra: "$",
        triggers: &["."],
        members_after: &["."],
        keywords: js_keywords!(),
        functions: js_functions!(),
        types: js_globals!(),
        variables: "console document window globalThis process module exports navigator localStorage",
        members: js_members!(),
        comments: &["//"],
        snippets: &[JS_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["typescript", "tsx"],
        extra: "$",
        triggers: &["."],
        members_after: &["."],
        keywords: concat!(
            js_keywords!(),
            " interface type enum implements private protected public readonly abstract declare namespace keyof as is any unknown never string number boolean symbol bigint object satisfies infer"
        ),
        functions: js_functions!(),
        types: concat!(js_globals!(), " Record Partial Required Readonly Pick Omit Exclude Extract ReturnType Parameters Awaited NonNullable"),
        variables: "console document window globalThis process module exports navigator localStorage",
        members: js_members!(),
        comments: &["//"],
        snippets: &[JS_SNIPPETS, TS_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["java"],
        triggers: &["."],
        members_after: &["."],
        keywords: "abstract assert boolean break byte case catch char class const continue default do double else enum extends final finally float for goto if implements import instanceof int interface long native new package private protected public return short static strictfp super switch synchronized this throw throws transient try void volatile while var record sealed permits yield true false null",
        types: "String Object Integer Long Double Float Boolean Character Byte Short Math System List ArrayList LinkedList Map HashMap TreeMap Set HashSet Optional StringBuilder Exception RuntimeException IllegalArgumentException IllegalStateException IOException Thread Runnable Arrays Collections Stream Collectors Iterator Scanner",
        members: "println printf print length size get put add remove contains containsKey isEmpty equals hashCode toString substring charAt indexOf split trim stream map filter collect forEach append toList of format valueOf parseInt",
        comments: &["//"],
        snippets: &[JAVA_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["c"],
        extra: "#",
        triggers: &[".", "->"],
        members_after: &[".", "->"],
        keywords: c_keywords!(),
        functions: c_functions!(),
        types: c_types!(),
        constants: "NULL EOF stdin stdout stderr EXIT_SUCCESS EXIT_FAILURE",
        comments: &["//"],
        snippets: &[C_SNIPPETS, C_COMMON_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["cpp"],
        extra: "#",
        triggers: &[".", "->", "::"],
        members_after: &[".", "->", "::"],
        keywords: concat!(
            c_keywords!(),
            " alignas alignof catch class concept consteval constexpr constinit const_cast co_await co_return co_yield decltype delete dynamic_cast explicit export final friend mutable namespace new noexcept nullptr operator override private protected public reinterpret_cast requires static_assert static_cast template this thread_local throw try typeid typename using virtual"
        ),
        functions: concat!(c_functions!(), " make_unique make_shared move forward swap sort find begin end getline to_string"),
        types: concat!(
            c_types!(),
            " std string vector map unordered_map set unordered_set pair tuple optional variant unique_ptr shared_ptr weak_ptr array deque list queue stack function string_view"
        ),
        constants: "NULL EOF cout cin cerr endl",
        members: "push_back emplace_back pop_back size empty clear begin end insert erase find at front back data c_str length substr reserve resize count get reset first second",
        paths: "cout cin cerr endl move forward swap sort find begin end make_unique make_shared to_string getline",
        comments: &["//"],
        snippets: &[CPP_SNIPPETS, C_COMMON_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["go"],
        triggers: &["."],
        members_after: &["."],
        keywords: "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var",
        functions: "append cap clear close complex copy delete imag len make max min new panic print println real recover",
        types: "bool byte complex64 complex128 error float32 float64 int int8 int16 int32 int64 rune string uint uint8 uint16 uint32 uint64 uintptr any comparable",
        constants: "true false nil iota",
        modules: "fmt os strings strconv errors io bufio time sync context http json sort math bytes filepath",
        members: "Println Printf Sprintf Sprint Errorf Fprintf Fprintln Contains Split Join TrimSpace HasPrefix HasSuffix ReplaceAll Itoa Atoi New Is As ReadFile WriteFile Open Create Exit Getenv Now Since Sleep Lock Unlock Add Done Wait Background WithCancel Marshal Unmarshal Slice Strings Ints",
        comments: &["//"],
        snippets: &[GO_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["bash"],
        triggers: &["$", "${"],
        keywords: "if then else elif fi case esac for select while until do done in function time return break continue exit local export readonly declare unset shift source alias set trap eval exec",
        functions: "echo printf read cd pwd ls cat grep sed awk find xargs sort uniq head tail wc cut tr mkdir rm cp mv touch chmod chown ln test tee curl wget git tar sudo kill ps jq sleep date basename dirname realpath mktemp which command type",
        variables: "HOME PATH PWD USER SHELL IFS RANDOM LINENO BASH_SOURCE FUNCNAME OLDPWD HOSTNAME EDITOR",
        comments: &["#"],
        snippets: &[BASH_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["sql"],
        triggers: &["."],
        members_after: &["."],
        ignore_case: true,
        keywords: "SELECT FROM WHERE AND OR NOT INSERT INTO VALUES UPDATE SET DELETE CREATE TABLE ALTER ADD COLUMN DROP INDEX VIEW JOIN INNER LEFT RIGHT FULL OUTER CROSS ON AS GROUP BY ORDER HAVING LIMIT OFFSET DISTINCT UNION ALL EXISTS IN BETWEEN LIKE IS NULL PRIMARY KEY FOREIGN REFERENCES DEFAULT UNIQUE CHECK CONSTRAINT CASE WHEN THEN ELSE END ASC DESC WITH RETURNING BEGIN COMMIT ROLLBACK TRANSACTION IF REPLACE TRUNCATE CASCADE",
        functions: "COUNT SUM AVG MIN MAX COALESCE NULLIF CAST ROUND LENGTH LOWER UPPER SUBSTRING TRIM NOW CONCAT ABS IFNULL ROW_NUMBER RANK STRFTIME",
        types: "INTEGER INT BIGINT SMALLINT TEXT VARCHAR CHAR BOOLEAN REAL FLOAT DOUBLE DECIMAL NUMERIC DATE TIME TIMESTAMP BLOB JSON SERIAL UUID",
        constants: "TRUE FALSE CURRENT_TIMESTAMP CURRENT_DATE",
        comments: &["--"],
        snippets: &[SQL_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["ruby"],
        extra: "@",
        triggers: &[".", "::"],
        members_after: &[".", "::"],
        keywords: "BEGIN END alias and begin break case class def defined? do else elsif end ensure false for if in module next nil not or redo rescue retry return self super then true undef unless until when while yield",
        functions: "require require_relative attr_accessor attr_reader attr_writer include extend private protected public raise puts print p pp lambda proc loop format sleep",
        types: "Array Hash String Integer Float Symbol Proc Struct Object Kernel Comparable Enumerable StandardError RuntimeError ArgumentError File Dir Time IO",
        members: "each map select reject reduce each_with_index each_with_object find include? empty? nil? length size first last push pop join split strip gsub sub upcase downcase to_s to_i to_f to_sym to_a keys values merge sort sort_by uniq flatten compact freeze new",
        paths: "new",
        comments: &["#"],
        snippets: &[RUBY_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["lua"],
        triggers: &[".", ":"],
        members_after: &[".", ":"],
        keywords: "and break do else elseif end false for function goto if in local nil not or repeat return then true until while",
        functions: "print type pairs ipairs tostring tonumber require pcall xpcall error assert select setmetatable getmetatable rawget rawset rawequal next unpack load dofile collectgarbage",
        modules: "string table math os io coroutine utf8 debug package",
        members: "format sub gsub find match gmatch len upper lower rep byte char insert remove concat sort unpack floor ceil max min abs random sqrt huge pi time clock date getenv execute open read write lines close",
        comments: &["--"],
        snippets: &[LUA_SNIPPETS],
        ..EMPTY
    },
    Lang {
        ids: &["php"],
        extra: "$",
        triggers: &["->", "::", "$"],
        members_after: &["->", "::"],
        keywords: "abstract and array as break callable case catch class clone const continue declare default do echo else elseif empty enddeclare endfor endforeach endif endswitch endwhile enum extends final finally fn for foreach function global goto if implements include include_once instanceof insteadof interface isset list match namespace new or print private protected public readonly require require_once return static switch throw trait try unset use var while xor yield true false null self parent",
        functions: "strlen str_replace str_contains str_starts_with substr strpos explode implode trim sprintf printf count array_map array_filter array_keys array_values array_merge array_push in_array array_key_exists json_encode json_decode var_dump print_r is_array is_string is_int is_null intval strval file_get_contents file_put_contents date time header htmlspecialchars preg_match preg_replace ucfirst strtolower strtoupper",
        variables: "$this $_GET $_POST $_SERVER $_SESSION $_COOKIE $_FILES $_ENV $GLOBALS",
        paths: "class",
        comments: &["//", "#"],
        snippets: &[PHP_SNIPPETS],
        ..EMPTY
    },
    Lang { ids: &["html"], extra: "-", triggers: &["<", "</"], in_strings: true, snippets: &[HTML_SNIPPETS], ..EMPTY },
    Lang { ids: &["css"], extra: "-@", triggers: &[":"], in_strings: true, ..EMPTY },
    Lang { ids: &["xml"], extra: "-", triggers: &["<", "</"], in_strings: true, ..EMPTY },
    Lang { ids: &["json"], in_strings: true, constants: "true false null", ..EMPTY },
    Lang { ids: &["yaml"], extra: "-", in_strings: true, constants: "true false null", comments: &["#"], ..EMPTY },
    Lang { ids: &["toml"], extra: "-", in_strings: true, constants: "true false", comments: &["#"], ..EMPTY },
    Lang { ids: &["markdown"], auto: false, in_strings: true, snippets: &[MARKDOWN_SNIPPETS], ..EMPTY },
];

fn lang(language: Option<&str>) -> &'static Lang {
    language.and_then(|id| LANGS.iter().find(|l| l.ids.contains(&id))).unwrap_or(&EMPTY)
}

const HTML_TAGS: &str = "a abbr address area article aside audio b base blockquote body br button canvas caption cite code col colgroup data datalist dd details dialog div dl dt em embed fieldset figcaption figure footer form h1 h2 h3 h4 h5 h6 head header hr html i iframe img input label legend li link main mark menu meta nav noscript object ol optgroup option output p picture pre progress q s samp script section select slot small source span strong style sub summary sup svg table tbody td template textarea tfoot th thead time title tr track u ul var video wbr";
const HTML_VOID: &str = "area base br col embed hr img input link meta source track wbr";
const HTML_ATTRIBUTES: &str = "id class style title href src alt type name value placeholder rel target width height lang charset content for action method onclick onchange oninput onsubmit onload tabindex role aria-label aria-hidden data- autocomplete download loading crossorigin srcset sizes min max step pattern maxlength rows cols colspan rowspan";
const HTML_BOOLEAN_ATTRIBUTES: &str = "disabled checked selected readonly required hidden autofocus async defer multiple open novalidate";

const CSS_PROPERTIES: &str = "align-content align-items align-self animation aspect-ratio background background-color background-image background-position background-repeat background-size border border-bottom border-collapse border-color border-left border-radius border-right border-style border-top border-width bottom box-shadow box-sizing color column-gap content cursor display filter flex flex-basis flex-direction flex-grow flex-shrink flex-wrap float font font-family font-size font-style font-weight gap grid grid-area grid-column grid-row grid-template-areas grid-template-columns grid-template-rows height inset justify-content justify-items left letter-spacing line-height list-style margin margin-bottom margin-left margin-right margin-top max-height max-width min-height min-width object-fit opacity outline overflow overflow-x overflow-y padding padding-bottom padding-left padding-right padding-top place-items pointer-events position right row-gap text-align text-decoration text-overflow text-shadow text-transform top transform transition user-select vertical-align visibility white-space width word-break z-index";
const CSS_VALUES: &str = "auto none inherit initial unset block inline inline-block flex inline-flex grid inline-grid contents hidden visible scroll absolute relative fixed sticky static center start end flex-start flex-end space-between space-around space-evenly stretch baseline row row-reverse column column-reverse wrap nowrap bold bolder lighter normal italic solid dashed dotted double pointer default text not-allowed grab transparent currentColor cover contain repeat no-repeat uppercase lowercase capitalize underline ease ease-in ease-out ease-in-out linear infinite black white red green blue gray grey orange yellow purple pink brown";
const CSS_FUNCTIONS: &str = "rgb rgba hsl hsla var calc url linear-gradient radial-gradient min max clamp translate translateX translateY rotate scale repeat minmax";
const CSS_PSEUDO: &str = "hover active focus focus-visible focus-within visited first-child last-child nth-child nth-of-type not is where has root before after placeholder disabled checked empty";
const CSS_AT_RULES: &str = "@media @import @keyframes @font-face @supports @layer @container @charset";

fn words(list: &'static str) -> impl Iterator<Item = &'static str> {
    list.split_whitespace()
}

// -- words ----------------------------------------------------------------------------------

pub fn is_word(c: char, extra: &str) -> bool {
    c.is_alphanumeric() || c == '_' || extra.contains(c)
}

/// The characters (besides letters, digits and `_`) that are part of a word in a language.
pub fn word_extra(language: Option<&str>) -> &'static str {
    lang(language).extra
}

/// Where the word that ends at `col` starts.
pub fn word_start(line: &str, col: usize, extra: &str) -> usize {
    let chars: Vec<char> = line.chars().take(col).collect();
    let mut start = chars.len();
    while start > 0 && is_word(chars[start - 1], extra) {
        start -= 1;
    }
    start
}

/// Does typing this character, with `before` the text left of the cursor, open the widget?
pub fn is_trigger(language: Option<&str>, before: &str) -> bool {
    let lang = lang(language);
    lang.triggers.iter().any(|trigger| {
        if !before.ends_with(trigger) {
            return false;
        }
        // `x.` and `f().` are member access; `1.` and `...` are not.
        if *trigger == "." {
            let rest = &before[..before.len() - 1];
            let previous = rest.chars().last();
            let token_start = word_start(rest, rest.chars().count(), lang.extra);
            let token: String = rest.chars().skip(token_start).collect();
            return matches!(previous, Some(c) if c == ')' || c == ']' || is_word(c, lang.extra))
                && !token.chars().next().map_or(false, |c| c.is_ascii_digit());
        }
        true
    })
}

/// Should the widget open by itself here (VS Code's quick suggestions)?
pub fn auto_suggest(language: Option<&str>) -> bool {
    lang(language).auto
}

pub fn ignore_case(language: Option<&str>) -> bool {
    lang(language).ignore_case
}

fn captures() -> &'static [&'static str] {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES.get_or_init(capture_names)
}

/// True inside a comment, or a string in a language where those are just text.
/// Looks at the character before `col` (the newest one may not be highlighted yet).
pub fn quiet_at(doc: &Doc, row: usize, col: usize) -> bool {
    if col == 0 {
        return false;
    }
    let Some(line) = doc.lines.get(row) else { return false };
    let Some(spans) = doc.spans.get(row) else { return false };
    let byte = line.char_indices().nth(col - 1).map(|(i, _)| i as u32).unwrap_or(line.len() as u32);
    let lang = lang(doc.language);
    if in_line_comment(lang.comments, &line.chars().take(col).collect::<String>()) {
        return true;
    }
    let in_strings = lang.in_strings;
    spans.iter().filter(|span| span.start <= byte && byte < span.end).any(|span| {
        let name = captures().get(span.id as usize).copied().unwrap_or("");
        name.starts_with("comment") || (!in_strings && name.starts_with("string"))
    })
}

/// Is the end of `before` inside a line comment? (Doesn't wait for the highlighter.)
fn in_line_comment(markers: &[&str], before: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in before.char_indices() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' || c == '`' => quote = Some(c),
            None if markers.iter().any(|m| before[i..].starts_with(m)) => return true,
            None => {}
        }
    }
    false
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Evidence {
    Seen,
    Capitalised,
    Used,
    Declared,
}

const FUNCTION_DECLARATIONS: &[&str] = &["def", "fn", "func", "function", "sub"];
const TYPE_DECLARATIONS: &[&str] = &["class", "struct", "enum", "interface", "trait", "type", "union", "module", "namespace", "record", "impl"];

/// Collect the words in `lines` with a guess at what each is.
fn scan(lines: &[String], rows: std::ops::Range<usize>, extra: &str, skip: Option<(usize, usize)>, out: &mut HashMap<String, (Kind, Evidence)>) {
    for row in rows {
        let chars: Vec<char> = lines[row].chars().collect();
        let mut previous_word: Option<String> = None;
        let mut i = 0;
        while i < chars.len() {
            if !is_word(chars[i], extra) {
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && is_word(chars[i], extra) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let declared_by = previous_word.replace(word.clone());
            if Some((row, start)) == skip || chars[start].is_ascii_digit() || word.chars().count() < 2 || out.len() >= MAX_WORDS {
                continue;
            }
            if !word.chars().any(char::is_alphanumeric) {
                continue;
            }
            let before = chars[..start].iter().rev().find(|c| !c.is_whitespace()).copied();
            let after = chars[i..].iter().find(|c| !c.is_whitespace()).copied();
            let member = matches!(before, Some('.')) || (start >= 2 && chars[start - 2..start] == ['-', '>']);
            let (kind, evidence) = match declared_by.as_deref() {
                Some(d) if FUNCTION_DECLARATIONS.contains(&d) => (Kind::Function, Evidence::Declared),
                Some(d) if TYPE_DECLARATIONS.contains(&d) => (Kind::Class, Evidence::Declared),
                _ if after == Some('(') => (if member { Kind::Method } else { Kind::Function }, Evidence::Used),
                _ if member => (Kind::Field, Evidence::Used),
                _ if word.len() > 1 && word.chars().all(|c| !c.is_lowercase()) && word.chars().any(char::is_uppercase) => {
                    (Kind::Constant, Evidence::Capitalised)
                }
                _ if word.chars().next().map_or(false, char::is_uppercase) => (Kind::Class, Evidence::Capitalised),
                _ => (Kind::Variable, Evidence::Seen),
            };
            match out.get_mut(&word) {
                Some(known) if known.1 >= evidence => {}
                Some(known) => *known = (kind, evidence),
                None => {
                    out.insert(word, (kind, evidence));
                }
            }
        }
    }
}

// -- building the list ----------------------------------------------------------------------

/// Where the widget is being opened.
pub struct Context<'a> {
    pub language: Option<&'static str>,
    pub lines: &'a [String],
    pub row: usize,
    /// Start of the word being completed (characters).
    pub start: usize,
    /// Other open documents in the same language.
    pub others: Vec<&'a [String]>,
}

struct Out {
    items: Vec<Item>,
    seen: HashSet<String>,
    ignore_case: bool,
    group: u8,
}

impl Out {
    fn key(&self, label: &str) -> String {
        if self.ignore_case {
            label.to_lowercase()
        } else {
            label.to_string()
        }
    }

    fn add(&mut self, mut item: Item) {
        item.group = self.group;
        // Snippets may share a label with a keyword ("for"); VS Code lists both.
        let key = format!("{}{}", if item.kind == Kind::Snippet { "\u{1}" } else { "" }, self.key(&item.label));
        if self.seen.insert(key) {
            self.items.push(item);
        }
    }

    fn list(&mut self, list: &'static str, kind: Kind, detail: &str) {
        for word in words(list) {
            self.add(Item::new(word, kind, detail, None));
        }
        self.group = self.group.saturating_add(1);
    }

    fn has(&self, label: &str) -> bool {
        self.seen.contains(&self.key(label))
    }
}

/// Everything that could be suggested at this spot (filtered later as you type).
pub fn candidates(cx: &Context) -> Vec<Item> {
    let lang = lang(cx.language);
    let line = &cx.lines[cx.row];
    let before: String = line.chars().take(cx.start).collect();
    let mut out = Out { items: Vec::new(), seen: HashSet::new(), ignore_case: lang.ignore_case, group: 0 };

    match cx.language {
        Some("html") if html(&before, &mut out) => return out.items,
        Some("xml") if before.ends_with('<') || before.ends_with("</") => {
            let closing = before.ends_with("</");
            let mut tags = HashSet::new();
            for text in cx.lines.iter().chain(cx.others.iter().flat_map(|lines| lines.iter())) {
                for piece in text.split('<').skip(1) {
                    let name: String = piece.chars().take_while(|c| is_word(*c, "-:.")).collect();
                    if !name.is_empty() {
                        tags.insert(name);
                    }
                }
            }
            let mut tags: Vec<String> = tags.into_iter().collect();
            tags.sort();
            for tag in tags {
                let body = if closing { format!("{tag}>") } else { format!("{tag}>$0</{tag}>") };
                out.add(Item::word(tag, Kind::Property, Some(body)));
            }
            return out.items;
        }
        Some("css") => css(cx, &before, &mut out),
        _ => {}
    }

    let member = lang.members_after.iter().any(|t| before.ends_with(t));
    let variable = cx.language == Some("bash") && (before.ends_with('$') || before.ends_with("${"));
    if variable {
        out.list(lang.variables, Kind::Variable, "variable");
    } else if before.ends_with("::") {
        out.list(lang.paths, Kind::Function, "associated item");
        out.list(lang.types, Kind::Class, "type");
        out.list(lang.constants, Kind::Constant, "constant");
    } else if member {
        out.list(lang.members, Kind::Method, "method");
    } else if cx.language != Some("css") {
        out.list(lang.keywords, Kind::Keyword, "keyword");
        out.list(lang.functions, Kind::Function, "built-in function");
        out.list(lang.types, Kind::Class, "built-in type");
        out.list(lang.constants, Kind::Constant, "constant");
        out.list(lang.modules, Kind::Module, "module");
        out.list(lang.variables, Kind::Variable, "variable");
        for group in lang.snippets {
            for (kind, label, body, detail) in group.iter() {
                out.add(Item::new(label, *kind, detail, Some(body)));
            }
        }
        if cx.language == Some("html") {
            // Emmet-style: a bare tag name expands to the element.
            for tag in words(HTML_TAGS) {
                let body = if words(HTML_VOID).any(|v| v == tag) { format!("<{tag}>") } else { format!("<{tag}>$0</{tag}>") };
                out.add(Item::new(tag, Kind::Snippet, "Emmet Abbreviation", Some(&body)));
            }
        }
    }

    // Words from the open documents of this language.
    out.group = out.group.saturating_add(1);
    let mut found = HashMap::new();
    let rows = cx.row.saturating_sub(SCAN_LINES)..(cx.row + SCAN_LINES).min(cx.lines.len());
    scan(cx.lines, rows, lang.extra, Some((cx.row, cx.start)), &mut found);
    for lines in &cx.others {
        scan(lines, 0..lines.len().min(SCAN_LINES), lang.extra, None, &mut found);
    }
    let text_only = matches!(cx.language, None | Some("markdown"));
    let mut found: Vec<(String, (Kind, Evidence))> = found.into_iter().collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    for (word, (kind, _)) in found {
        if !out.has(&word) {
            let kind = if text_only { Kind::Text } else { kind };
            out.add(Item::word(word, kind, None));
        }
    }
    out.items
}

/// Tags after `<`, attributes inside a tag. Returns true when that is all there is to offer.
fn html(before: &str, out: &mut Out) -> bool {
    if before.ends_with("</") {
        for tag in words(HTML_TAGS).filter(|t| !words(HTML_VOID).any(|v| v == *t)) {
            out.add(Item::new(tag, Kind::Property, "closing tag", Some(&format!("{tag}>"))));
        }
        return true;
    }
    if before.ends_with('<') {
        for tag in words(HTML_TAGS) {
            let body = if words(HTML_VOID).any(|v| v == tag) { format!("{tag}>") } else { format!("{tag}>$0</{tag}>") };
            out.add(Item::new(tag, Kind::Property, "element", Some(&body)));
        }
        return true;
    }
    // Inside an opening tag, outside any quoted value: attributes.
    let open = before.rfind('<');
    let close = before.rfind('>');
    if let Some(open) = open.filter(|o| close.map_or(true, |c| c < *o)) {
        let inside = &before[open..];
        let quotes = inside.matches('"').count() + inside.matches('\'').count();
        if inside.contains(char::is_whitespace) && !inside.starts_with("</") && quotes % 2 == 0 {
            for attribute in words(HTML_ATTRIBUTES) {
                let body = if attribute.ends_with('-') { format!("{attribute}$1=\"$0\"") } else { format!("{attribute}=\"$0\"") };
                out.add(Item::new(attribute, Kind::Value, "attribute", Some(&body)));
            }
            out.list(HTML_BOOLEAN_ATTRIBUTES, Kind::Value, "attribute");
            return true;
        }
    }
    false
}

/// Selectors and at-rules outside a block; properties, then values, inside one.
fn css(cx: &Context, before: &str, out: &mut Out) {
    let mut depth = 0i32;
    for text in cx.lines[..cx.row].iter().map(String::as_str).chain(std::iter::once(before)) {
        depth += text.matches('{').count() as i32 - text.matches('}').count() as i32;
    }
    let statement = before.rsplit(|c| c == ';' || c == '{' || c == '}').next().unwrap_or("");
    if depth <= 0 {
        if before.ends_with(':') {
            out.list(CSS_PSEUDO, Kind::Keyword, "pseudo-class");
        } else {
            out.list(CSS_AT_RULES, Kind::Keyword, "at-rule");
            out.list(HTML_TAGS, Kind::Property, "element");
        }
    } else if statement.contains(':') {
        out.list(CSS_VALUES, Kind::Value, "value");
        for function in words(CSS_FUNCTIONS) {
            out.add(Item::new(function, Kind::Function, "function", Some(&format!("{function}($0)"))));
        }
        out.add(Item::new("!important", Kind::Keyword, "keyword", None));
    } else {
        for property in words(CSS_PROPERTIES) {
            out.add(Item::new(property, Kind::Property, "property", Some(&format!("{property}: $0;"))));
        }
    }
}

// -- snippets ---------------------------------------------------------------------------------

/// Expand a snippet body. `\t` becomes four spaces, new lines keep `indent`, and the
/// result says which stretch to select first (the lowest tab stop, else `$0`), in
/// characters from the start of the text.
pub fn expand_snippet(body: &str, indent: &str) -> (String, Option<(usize, usize)>) {
    let chars: Vec<char> = body.chars().collect();
    let mut text = String::new();
    let mut len = 0usize;
    let mut stops: Vec<(u32, usize, usize)> = Vec::new();
    let mut defaults: HashMap<u32, String> = HashMap::new();
    let push = |text: &mut String, len: &mut usize, s: &str| {
        text.push_str(s);
        *len += s.chars().count();
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() => {
                push(&mut text, &mut len, &chars[i + 1].to_string());
                i += 2;
            }
            '$' if chars.get(i + 1).map_or(false, |d| d.is_ascii_digit()) => {
                let digits: String = chars[i + 1..].iter().take_while(|d| d.is_ascii_digit()).collect();
                let number: u32 = digits.parse().unwrap_or(0);
                let mirrored = defaults.get(&number).cloned().unwrap_or_default();
                let at = len;
                push(&mut text, &mut len, &mirrored);
                if !stops.iter().any(|(n, _, _)| *n == number) {
                    stops.push((number, at, len));
                }
                i += 1 + digits.len();
            }
            '$' if chars.get(i + 1) == Some(&'{') => {
                let digits: String = chars[i + 2..].iter().take_while(|d| d.is_ascii_digit()).collect();
                let number: u32 = digits.parse().unwrap_or(0);
                let mut j = i + 2 + digits.len();
                let mut default = String::new();
                if chars.get(j) == Some(&':') {
                    j += 1;
                    while j < chars.len() && chars[j] != '}' {
                        if chars[j] == '\\' && j + 1 < chars.len() {
                            j += 1;
                        }
                        default.push(chars[j]);
                        j += 1;
                    }
                }
                let at = len;
                push(&mut text, &mut len, &default);
                if !stops.iter().any(|(n, _, _)| *n == number) {
                    stops.push((number, at, len));
                }
                defaults.insert(number, default);
                i = j + 1;
            }
            '\t' => {
                push(&mut text, &mut len, "    ");
                i += 1;
            }
            '\n' => {
                push(&mut text, &mut len, "\n");
                push(&mut text, &mut len, indent);
                i += 1;
            }
            _ => {
                push(&mut text, &mut len, &c.to_string());
                i += 1;
            }
        }
    }
    let first = stops.iter().filter(|(n, _, _)| *n > 0).min_by_key(|(n, _, _)| *n).or_else(|| stops.iter().find(|(n, _, _)| *n == 0));
    (text, first.map(|(_, start, end)| (*start, *end)))
}

/// What to insert for an item, given the text typed so far.
pub fn insert_text(item: &Item, typed: &str, indent: &str, ignore_case: bool) -> (String, Option<(usize, usize)>) {
    // SQL: `sel` + Enter gives `select`, `SEL` gives `SELECT`.
    let lower = ignore_case && item.builtin && typed.chars().any(char::is_alphabetic) && !typed.chars().any(char::is_uppercase);
    let fold = |s: &str| if lower { s.to_lowercase() } else { s.to_string() };
    match &item.body {
        Some(body) => expand_snippet(&fold(body), indent),
        None => (fold(&item.label), None),
    }
}

// -- the widget ---------------------------------------------------------------------------------

/// The suggest widget, open under the cursor of one document.
pub struct Suggest {
    pub doc: u64,
    pub row: usize,
    /// Column where the completed word starts.
    pub start: usize,
    /// Opened by a trigger character or Ctrl+Space: stays open with nothing typed.
    pub explicit: bool,
    items: Vec<Item>,
    /// Matching items (index into `items`) and the matched character positions.
    shown: Vec<(usize, Vec<usize>)>,
    pub selected: usize,
    /// Rank `sel` and `SEL` the same (SQL).
    pub ignore_case: bool,
    top: usize,
    rect: Rect,
}

impl Suggest {
    pub fn new(doc: u64, row: usize, start: usize, explicit: bool, items: Vec<Item>) -> Suggest {
        Suggest { doc, row, start, explicit, items, shown: Vec::new(), selected: 0, ignore_case: false, top: 0, rect: Rect::default() }
    }

    /// Narrow the list to what matches `typed`. False when nothing is worth showing.
    pub fn filter(&mut self, typed: &str) -> bool {
        let mut scored: Vec<(i32, usize, Vec<usize>)> = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            if typed.is_empty() {
                scored.push((0, index, Vec::new()));
                continue;
            }
            let Some((mut score, positions)) = fuzzy(typed, &item.label) else { continue };
            // The first letter typed has to start the label or one of its parts (VS Code does the same).
            let label: Vec<char> = item.label.chars().collect();
            let p = positions[0];
            let boundary = p == 0
                || !label[p - 1].is_alphanumeric()
                || (label[p].is_uppercase() && label[p - 1].is_lowercase());
            if !boundary {
                continue;
            }
            let same = |a: &str, b: &str| if self.ignore_case { a.eq_ignore_ascii_case(b) } else { a == b };
            if same(&item.label, typed) {
                score += 60;
            } else if item.label.starts_with(typed) || (self.ignore_case && item.label.to_lowercase().starts_with(&typed.to_lowercase())) {
                score += 30;
            } else if item.label.to_lowercase().starts_with(&typed.to_lowercase()) {
                score += 15;
            }
            scored.push((score, index, positions));
        }
        let items = &self.items;
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| if typed.is_empty() { items[a.1].group.cmp(&items[b.1].group) } else { std::cmp::Ordering::Equal })
                .then_with(|| items[a.1].label.to_lowercase().cmp(&items[b.1].label.to_lowercase()))
                .then_with(|| a.1.cmp(&b.1))
        });
        scored.truncate(500);
        self.shown = scored.into_iter().map(|(_, index, positions)| (index, positions)).collect();
        self.selected = 0;
        self.top = 0;
        // A word typed out in full, with nothing else to offer, needs no popup (and must
        // not swallow the Enter that follows it).
        if let [(index, _)] = self.shown.as_slice() {
            let item = &self.items[*index];
            if item.body.is_none() && (item.label == typed || (self.ignore_case && item.label.eq_ignore_ascii_case(typed))) {
                return false;
            }
        }
        !self.shown.is_empty()
    }

    pub fn current(&self) -> Option<&Item> {
        self.shown.get(self.selected).map(|(index, _)| &self.items[*index])
    }

    #[cfg(test)]
    pub fn labels(&self) -> Vec<&str> {
        self.shown.iter().map(|(index, _)| self.items[*index].label.as_str()).collect()
    }

    fn visible(&self) -> usize {
        self.shown.len().min(VISIBLE)
    }

    /// Move the highlight; wraps around at either end like VS Code.
    pub fn step(&mut self, delta: i32) {
        let count = self.shown.len() as i32;
        if count == 0 {
            return;
        }
        let page = delta.abs() > 1;
        let target = self.selected as i32 + delta;
        self.selected = if page { target.clamp(0, count - 1) } else { target.rem_euclid(count) } as usize;
    }

    pub fn page(&self) -> i32 {
        self.visible().max(1) as i32
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.rect.contains(x, y)
    }

    pub fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let inside = x > self.rect.x && x < self.rect.right() - 1 && y > self.rect.y && y < self.rect.bottom() - 1;
        let index = self.top + (y - self.rect.y - 1).max(0) as usize;
        (inside && index < self.shown.len()).then_some(index)
    }

    pub fn scroll(&mut self, delta: i32) {
        let max = self.shown.len().saturating_sub(self.visible()) as i32;
        self.top = (self.top as i32 + delta).clamp(0, max) as usize;
        self.selected = self.selected.clamp(self.top, self.top + self.visible().saturating_sub(1));
    }

    /// Paint the widget for a word starting at screen cell `anchor`, inside `bounds`.
    pub fn draw(&mut self, buf: &mut Buffer, t: &VSTheme, anchor: (i32, i32), bounds: Rect, mouse: (i32, i32)) {
        let visible = self.visible();
        if visible == 0 || bounds.w < 12 || bounds.h < 3 {
            self.rect = Rect::default();
            return;
        }
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + visible {
            self.top = self.selected + 1 - visible;
        }
        let label_w = self.shown.iter().map(|(i, _)| width(&self.items[*i].label)).max().unwrap_or(0).min(40);
        let detail_w = self.shown.iter().map(|(i, _)| width(&self.items[*i].detail)).max().unwrap_or(0).min(26);
        let w = (label_w + if detail_w > 0 { detail_w + 3 } else { 0 } + 6).clamp(26, 70).min(bounds.w);
        let h = (visible as i32 + 2).min(bounds.h);
        // Icon and a space sit left of the word, so the labels line up with what you typed.
        let x = (anchor.0 - 4).min(bounds.right() - w).max(bounds.x);
        let below = anchor.1 + 1;
        let y = if below + h <= bounds.bottom() {
            below
        } else if anchor.1 - h >= bounds.y {
            anchor.1 - h
        } else {
            (bounds.bottom() - h).max(bounds.y)
        };
        self.rect = Rect::new(x, y, w, h);
        let bg = t.c("editorSuggestWidget.background");
        let fg = t.c("editorSuggestWidget.foreground");
        buf.fill(self.rect, Style::new(fg, bg));
        buf.round_box(self.rect, t.c("editorSuggestWidget.border"));
        let rows = (h - 2).max(0) as usize;
        for (n, (index, positions)) in self.shown.iter().enumerate().skip(self.top).take(rows) {
            let item = &self.items[*index];
            let ry = y + 1 + (n - self.top) as i32;
            let row = Rect::new(x + 1, ry, w - 2, 1);
            let selected = n == self.selected;
            let (row_fg, highlight) = if selected {
                buf.fill(row, Style::new(fg, t.c("editorSuggestWidget.selectedBackground")));
                (t.c("editorSuggestWidget.selectedForeground"), t.c("editorSuggestWidget.focusHighlightForeground"))
            } else {
                if row.contains(mouse.0, mouse.1) {
                    buf.fill(row, Style::new(fg, t.c("list.hoverBackground")));
                }
                (fg, t.c("editorSuggestWidget.highlightForeground"))
            };
            let icon = if t.mono_icons { t.c("icon.monoForeground") } else { t.c(item.kind.colour_key()) };
            buf.print(x + 2, ry, item.kind.glyph(), icon, 0, x + 4);
            let right = row.right() - 1;
            let mut cx = x + 4;
            for (i, c) in item.label.chars().enumerate() {
                let hit = positions.contains(&i);
                let mut s = [0u8; 4];
                cx = buf.print(cx, ry, c.encode_utf8(&mut s), if hit { highlight } else { row_fg }, if hit { BOLD } else { 0 }, right);
            }
            if selected && !item.detail.is_empty() {
                let room = right - cx - 2;
                if room >= 4 {
                    let detail = ellipsize(&item.detail, room);
                    buf.print(right - width(&detail), ry, &detail, t.c("descriptionForeground"), 0, right);
                }
            }
        }
        // A thumb on the right edge when the list scrolls.
        let total = self.shown.len();
        if total > rows && rows > 0 {
            let size = (rows * rows / total).max(1);
            let offset = (self.top * (rows - size)) / (total - rows).max(1);
            for i in 0..size {
                buf.print(x + w - 1, y + 1 + (offset + i) as i32, "┃", t.c("scrollbarSlider.hoverBackground"), 0, x + w);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_string).collect()
    }

    fn suggest(language: &str, text: &str) -> Vec<String> {
        let lines = lines(text);
        let row = lines.len() - 1;
        let col = lines[row].chars().count();
        let extra = word_extra(Some(language));
        let start = word_start(&lines[row], col, extra);
        let language: &'static str = LANGS.iter().flat_map(|l| l.ids.iter()).find(|id| **id == language).copied().unwrap_or("python");
        let items = candidates(&Context { language: Some(language), lines: &lines, row, start, others: Vec::new() });
        let mut widget = Suggest::new(1, row, start, true, items);
        let typed: String = lines[row].chars().skip(start).collect();
        widget.filter(&typed);
        widget.labels().into_iter().map(str::to_string).collect()
    }

    #[test]
    fn every_language_has_suggestions() {
        let cases = [
            ("python", "pri", "print"),
            ("rust", "prin", "println!"),
            ("javascript", "cons", "console"),
            ("typescript", "inter", "interface"),
            ("tsx", "Parti", "Partial"),
            ("java", "Strin", "String"),
            ("c", "mall", "malloc"),
            ("cpp", "nullp", "nullptr"),
            ("go", "fun", "func"),
            ("bash", "ech", "echo"),
            ("sql", "sel", "SELECT"),
            ("ruby", "requ", "require"),
            ("lua", "ipa", "ipairs"),
            ("php", "str_rep", "str_replace"),
            ("html", "<di", "div"),
            ("css", "a {\n  backg", "background"),
            ("json", "{\"a\": tr", "true"),
            ("yaml", "on: fal", "false"),
            ("toml", "x = tr", "true"),
            ("markdown", "code", "codeblock"),
            ("xml", "<note><to/>\n<n", "note"),
        ];
        for (language, text, expected) in cases {
            let got = suggest(language, text);
            assert!(got.iter().any(|l| l == expected), "{language}: {text:?} -> {got:?}");
        }
    }

    #[test]
    fn document_words_are_suggested_with_a_kind() {
        let text = "def compute_total(items):\n    return items\nclass Basket:\n    pass\ncomp";
        let lines = lines(text);
        let items = candidates(&Context { language: Some("python"), lines: &lines, row: 4, start: 0, others: Vec::new() });
        let total = items.iter().find(|i| i.label == "compute_total").expect("word from the document");
        assert_eq!(total.kind, Kind::Function);
        let basket = items.iter().find(|i| i.label == "Basket").unwrap();
        assert_eq!(basket.kind, Kind::Class);
        // The word being typed is not suggested to itself.
        assert!(!items.iter().any(|i| i.label == "comp"));
    }

    #[test]
    fn members_after_a_dot() {
        let got = suggest("python", "names = []\nnames.app");
        assert_eq!(got.first().map(String::as_str), Some("append"));
        assert!(is_trigger(Some("python"), "names."));
        assert!(!is_trigger(Some("python"), "x = 1."));
        assert!(is_trigger(Some("rust"), "String::"));
        assert!(is_trigger(Some("cpp"), "p->"));
    }

    #[test]
    fn html_and_css_context() {
        let attrs = suggest("html", "<a hr");
        assert_eq!(attrs.first().map(String::as_str), Some("href"));
        let values = suggest("css", "a {\n  display: fl");
        assert!(values.iter().any(|v| v == "flex"));
        let pseudo = suggest("css", "a:hov");
        assert!(pseudo.iter().any(|v| v == "hover"));
    }

    #[test]
    fn snippets_expand_with_indent_and_first_stop() {
        let (text, stop) = expand_snippet("def ${1:name}($2):\n\t${0:pass}", "    ");
        assert_eq!(text, "def name():\n        pass");
        assert_eq!(stop, Some((4, 8)));
        let (text, stop) = expand_snippet("for (int ${1:i} = 0; $1 < ${2:n}; $1++) {\n\t$0\n}", "");
        assert_eq!(text, "for (int i = 0; i < n; i++) {\n    \n}");
        assert_eq!(stop, Some((9, 10)));
        let (text, _) = expand_snippet("foreach (${1:\\$items} as \\$x)", "");
        assert_eq!(text, "foreach ($items as $x)");
    }

    #[test]
    fn line_comments_are_quiet() {
        assert!(in_line_comment(&["//"], "let x = 1; // pri"));
        assert!(!in_line_comment(&["//"], "let url = \"http://x\"; pri"));
        assert!(in_line_comment(&["#"], "x = 1  # pri"));
        assert!(!in_line_comment(&["#"], "s = '#'; pri"));
        let got = suggest("rust", "let s = String::");
        assert_eq!(got.first().map(String::as_str), Some("collections"));
        assert!(got.iter().any(|l| l == "new"));
    }

    #[test]
    fn sql_follows_the_case_you_type() {
        let item = Item::new("SELECT", Kind::Keyword, "keyword", None);
        assert_eq!(insert_text(&item, "sel", "", true).0, "select");
        assert_eq!(insert_text(&item, "SEL", "", true).0, "SELECT");
        let lines = lines("sel");
        let items = candidates(&Context { language: Some("sql"), lines: &lines, row: 0, start: 0, others: Vec::new() });
        let mut widget = Suggest::new(1, 0, 0, false, items);
        widget.ignore_case = true;
        widget.filter("sel");
        assert_eq!(widget.current().map(|i| i.label.as_str()), Some("SELECT"));
    }

    #[test]
    fn a_fully_typed_word_closes_the_widget() {
        let lines = lines("subtotal = 1\nsubtotal");
        let items = candidates(&Context { language: Some("python"), lines: &lines, row: 1, start: 0, others: Vec::new() });
        let mut widget = Suggest::new(1, 1, 0, false, items);
        assert!(!widget.filter("subtotal"));
        assert!(widget.filter("subt"));
    }
}
