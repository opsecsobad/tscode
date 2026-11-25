"""Code completion: VS Code's suggest widget, without language servers.

Suggestions come from two places:

* per-language tables: keywords, built-in functions / types / constants, common
  modules, common methods (offered after `.`, `->`, `::`...) and snippets;
* the words in the open documents of the same language (VS Code's word-based
  suggestions), with a little guessing about what each one is: a name after
  `def` / `fn` / `function` is a function, after `class` / `struct` a class, a name
  followed by `(` is called, ALL_CAPS is a constant...

HTML and CSS get context of their own (tags, attributes, properties, values), and XML
suggests the tag names already used in the file.

`candidates` builds the list once when the widget opens; `Suggest.filter` narrows it
on every key press. The widget itself is `widgets/suggest.py`.

The tables are shared with the Rust build (rust/src/completion.rs); keep them in step.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from enum import Enum

from .icons import ASCII

VISIBLE = 12  # rows shown before the list scrolls (VS Code shows 12)
SCAN_LINES = 10_000  # lines either side of the cursor scanned for words
MAX_WORDS = 20_000


class Kind(Enum):
    # value: (codicon, ASCII stand-in, theme key for the icon colour)
    SNIPPET = ("\ueb66", "s", "symbolIcon.snippetForeground")
    KEYWORD = ("\ueb62", "k", "symbolIcon.keywordForeground")
    FUNCTION = ("\uea8c", "f", "symbolIcon.functionForeground")
    METHOD = ("\uea8c", "f", "symbolIcon.methodForeground")
    CLASS = ("\ueb5b", "c", "symbolIcon.classForeground")
    MODULE = ("\uea8b", "m", "symbolIcon.moduleForeground")
    VARIABLE = ("\uea88", "v", "symbolIcon.variableForeground")
    FIELD = ("\ueb5f", "v", "symbolIcon.fieldForeground")
    PROPERTY = ("\ueb65", "p", "symbolIcon.propertyForeground")
    CONSTANT = ("\ueb5d", "C", "symbolIcon.constantForeground")
    VALUE = ("\ueb5e", "e", "symbolIcon.enumeratorMemberForeground")
    TEXT = ("\uea93", "t", "symbolIcon.textForeground")

    @property
    def glyph(self) -> str:
        return self.value[1] if ASCII else self.value[0]

    @property
    def colour_key(self) -> str:
        return self.value[2]


S = Kind.SNIPPET
F = Kind.FUNCTION

Snippet = tuple[Kind, str, str, str]  # (kind, label, body, detail)


@dataclass
class Item:
    """One suggestion."""

    label: str
    kind: Kind
    detail: str = ""  # shown on the right of the selected row
    body: str | None = None  # snippet text ($1, ${1:default}, $0) inserted instead of the label
    builtin: bool = True  # from a language table (SQL keywords follow the case you type)
    group: int = 0  # list it came from; with nothing typed, earlier groups come first


@dataclass
class Lang:
    ids: list[str] = field(default_factory=list)
    extra: str = ""  # characters that belong to a word besides letters, digits and _
    triggers: list[str] = field(default_factory=list)  # text before the cursor that opens the widget
    members_after: list[str] = field(default_factory=list)  # ...after which methods are offered
    auto: bool = True  # open while typing (VS Code leaves it off for Markdown)
    in_strings: bool = False  # suggest inside strings too (data and markup languages)
    ignore_case: bool = False  # keywords match and insert in the case you type (SQL)
    keywords: str = ""
    functions: str = ""
    types: str = ""
    constants: str = ""
    modules: str = ""
    variables: str = ""
    members: str = ""
    paths: str = ""  # offered after ::
    comments: list[str] = field(default_factory=list)  # line comment markers
    snippets: list[list[Snippet]] = field(default_factory=list)


EMPTY = Lang()

# -- language tables (generated from rust/src/completion.rs) --------------------------------

JS_KEYWORDS = "break case catch class const continue debugger default delete do else export extends false finally for function if import in instanceof let new null return super switch this throw true try typeof undefined var void while with yield async await of static get set"

JS_GLOBALS = "Array Object String Number Boolean Promise Date RegExp Map Set WeakMap WeakSet Error TypeError Symbol BigInt Math JSON Intl Reflect Proxy"

JS_FUNCTIONS = "parseInt parseFloat isNaN isFinite setTimeout setInterval clearTimeout clearInterval fetch require structuredClone queueMicrotask encodeURIComponent decodeURIComponent"

JS_MEMBERS = "log error warn info debug table length push pop shift unshift slice splice concat join reverse sort map filter reduce forEach find findIndex some every includes indexOf flat flatMap fill keys values entries then catch finally toString toFixed split trim trimStart trimEnd replace replaceAll startsWith endsWith toLowerCase toUpperCase padStart padEnd charAt substring match test bind call apply addEventListener removeEventListener querySelector querySelectorAll getElementById createElement appendChild remove classList textContent innerHTML style setAttribute getAttribute preventDefault stopPropagation json text stringify parse assign freeze floor ceil round random max min abs"

C_KEYWORDS = "auto break case char const continue default do double else enum extern float for goto if inline int long register restrict return short signed sizeof static struct switch typedef union unsigned void volatile while bool true false #define #ifdef #ifndef #endif #if #elif #else #pragma #undef"

C_FUNCTIONS = "printf fprintf sprintf snprintf scanf sscanf puts putchar getchar fgets fputs fopen fclose fread fwrite fflush malloc calloc realloc free memcpy memmove memset memcmp strlen strcpy strncpy strcat strcmp strncmp strchr strrchr strstr strdup strtok atoi atof strtol strtoul exit abort assert qsort bsearch rand srand time perror"

C_TYPES = "size_t ssize_t FILE int8_t int16_t int32_t int64_t uint8_t uint16_t uint32_t uint64_t uintptr_t intptr_t ptrdiff_t"

HTML_TAGS = "a abbr address area article aside audio b base blockquote body br button canvas caption cite code col colgroup data datalist dd details dialog div dl dt em embed fieldset figcaption figure footer form h1 h2 h3 h4 h5 h6 head header hr html i iframe img input label legend li link main mark menu meta nav noscript object ol optgroup option output p picture pre progress q s samp script section select slot small source span strong style sub summary sup svg table tbody td template textarea tfoot th thead time title tr track u ul var video wbr"

HTML_VOID = "area base br col embed hr img input link meta source track wbr"

HTML_ATTRIBUTES = "id class style title href src alt type name value placeholder rel target width height lang charset content for action method onclick onchange oninput onsubmit onload tabindex role aria-label aria-hidden data- autocomplete download loading crossorigin srcset sizes min max step pattern maxlength rows cols colspan rowspan"

HTML_BOOLEAN_ATTRIBUTES = "disabled checked selected readonly required hidden autofocus async defer multiple open novalidate"

CSS_PROPERTIES = "align-content align-items align-self animation aspect-ratio background background-color background-image background-position background-repeat background-size border border-bottom border-collapse border-color border-left border-radius border-right border-style border-top border-width bottom box-shadow box-sizing color column-gap content cursor display filter flex flex-basis flex-direction flex-grow flex-shrink flex-wrap float font font-family font-size font-style font-weight gap grid grid-area grid-column grid-row grid-template-areas grid-template-columns grid-template-rows height inset justify-content justify-items left letter-spacing line-height list-style margin margin-bottom margin-left margin-right margin-top max-height max-width min-height min-width object-fit opacity outline overflow overflow-x overflow-y padding padding-bottom padding-left padding-right padding-top place-items pointer-events position right row-gap text-align text-decoration text-overflow text-shadow text-transform top transform transition user-select vertical-align visibility white-space width word-break z-index"

CSS_VALUES = "auto none inherit initial unset block inline inline-block flex inline-flex grid inline-grid contents hidden visible scroll absolute relative fixed sticky static center start end flex-start flex-end space-between space-around space-evenly stretch baseline row row-reverse column column-reverse wrap nowrap bold bolder lighter normal italic solid dashed dotted double pointer default text not-allowed grab transparent currentColor cover contain repeat no-repeat uppercase lowercase capitalize underline ease ease-in ease-out ease-in-out linear infinite black white red green blue gray grey orange yellow purple pink brown"

CSS_FUNCTIONS = "rgb rgba hsl hsla var calc url linear-gradient radial-gradient min max clamp translate translateX translateY rotate scale repeat minmax"

CSS_PSEUDO = "hover active focus focus-visible focus-within visited first-child last-child nth-child nth-of-type not is where has root before after placeholder disabled checked empty"

CSS_AT_RULES = "@media @import @keyframes @font-face @supports @layer @container @charset"

PYTHON_SNIPPETS: list[Snippet] = [
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
]

RUST_SNIPPETS: list[Snippet] = [
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
]

JS_SNIPPETS: list[Snippet] = [
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
]

TS_SNIPPETS: list[Snippet] = [
    (S, "interface", "interface ${1:Name} {\n\t$0\n}", "Interface Declaration"),
    (S, "type", "type ${1:Name} = $0;", "Type Alias"),
    (S, "enum", "enum ${1:Name} {\n\t$0\n}", "Enum Declaration"),
]

JAVA_SNIPPETS: list[Snippet] = [
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
]

C_COMMON_SNIPPETS: list[Snippet] = [
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
]

C_SNIPPETS: list[Snippet] = [(S, "main", "int main(int argc, char *argv[]) {\n\t$0\n\treturn 0;\n}", "main function")]

CPP_SNIPPETS: list[Snippet] = [
    (S, "main", "int main() {\n\t$0\n\treturn 0;\n}", "main function"),
    (S, "class", "class ${1:Name} {\npublic:\n\t$1();\n\t~$1();\n\nprivate:\n\t$0\n};", "class"),
    (S, "namespace", "namespace ${1:name} {\n\n$0\n\n}", "namespace"),
    (S, "forr", "for (auto& ${1:item} : ${2:items}) {\n\t$0\n}", "range-based for loop"),
    (S, "cout", "std::cout << $1 << std::endl;", "Print to standard out"),
    (S, "try", "try {\n\t$1\n} catch (const ${2:std::exception}& e) {\n\t$0\n}", "try/catch"),
]

GO_SNIPPETS: list[Snippet] = [
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
]

BASH_SNIPPETS: list[Snippet] = [
    (S, "if", "if [[ ${1:condition} ]]; then\n\t$0\nfi", "if statement"),
    (S, "ifelse", "if [[ ${1:condition} ]]; then\n\t$2\nelse\n\t$0\nfi", "if/else statement"),
    (S, "for", "for ${1:item} in ${2:items}; do\n\t$0\ndone", "for loop"),
    (S, "while", "while ${1:condition}; do\n\t$0\ndone", "while loop"),
    (S, "case", "case ${1:word} in\n\t${2:pattern})\n\t\t$0\n\t\t;;\nesac", "case statement"),
    (S, "function", "${1:name}() {\n\t$0\n}", "function"),
    (S, "shebang", "#!/usr/bin/env bash\n$0", "Shebang line"),
    (S, "strict", "set -euo pipefail\n$0", "Strict mode"),
]

SQL_SNIPPETS: list[Snippet] = [
    (S, "select-from", "SELECT ${1:*}\nFROM ${2:table}\nWHERE ${3:condition};", "SELECT statement"),
    (S, "insert-into", "INSERT INTO ${1:table} (${2:columns})\nVALUES (${3:values});", "INSERT statement"),
    (S, "update-set", "UPDATE ${1:table}\nSET ${2:column} = ${3:value}\nWHERE ${4:condition};", "UPDATE statement"),
    (S, "delete-from", "DELETE FROM ${1:table}\nWHERE ${2:condition};", "DELETE statement"),
    (S, "create-table", "CREATE TABLE ${1:name} (\n\t${2:id} INTEGER PRIMARY KEY,\n\t$0\n);", "CREATE TABLE statement"),
]

RUBY_SNIPPETS: list[Snippet] = [
    (S, "def", "def ${1:name}$2\n\t$0\nend", "Method definition"),
    (S, "class", "class ${1:Name}\n\tdef initialize$2\n\t\t$0\n\tend\nend", "Class definition"),
    (S, "module", "module ${1:Name}\n\t$0\nend", "Module definition"),
    (S, "if", "if ${1:condition}\n\t$0\nend", "if statement"),
    (S, "unless", "unless ${1:condition}\n\t$0\nend", "unless statement"),
    (S, "each", "each do |${1:item}|\n\t$0\nend", "each block"),
    (S, "do", "do |${1:args}|\n\t$0\nend", "do block"),
    (S, "case", "case ${1:value}\nwhen ${2:x}\n\t$0\nend", "case statement"),
    (S, "begin", "begin\n\t$1\nrescue ${2:StandardError} => e\n\t$0\nend", "begin/rescue"),
]

LUA_SNIPPETS: list[Snippet] = [
    (S, "function", "function ${1:name}($2)\n\t$0\nend", "function"),
    (S, "lfunction", "local function ${1:name}($2)\n\t$0\nend", "local function"),
    (S, "if", "if ${1:condition} then\n\t$0\nend", "if statement"),
    (S, "for", "for ${1:i} = ${2:1}, ${3:n} do\n\t$0\nend", "numeric for loop"),
    (S, "fori", "for ${1:i}, ${2:v} in ipairs(${3:t}) do\n\t$0\nend", "for ipairs"),
    (S, "forp", "for ${1:k}, ${2:v} in pairs(${3:t}) do\n\t$0\nend", "for pairs"),
    (S, "while", "while ${1:condition} do\n\t$0\nend", "while loop"),
    (S, "repeat", "repeat\n\t$0\nuntil ${1:condition}", "repeat loop"),
    (S, "req", "local ${1:mod} = require(\"${2:mod}\")", "require a module"),
]

PHP_SNIPPETS: list[Snippet] = [
    (S, "php", "<?php\n\n$0", "PHP opening tag"),
    (S, "function", "function ${1:name}($2)\n{\n\t$0\n}", "function"),
    (S, "class", "class ${1:Name}\n{\n\tpublic function __construct($2)\n\t{\n\t\t$0\n\t}\n}", "class"),
    (S, "if", "if (${1:condition}) {\n\t$0\n}", "if statement"),
    (S, "foreach", "foreach (${1:\\$items} as ${2:\\$item}) {\n\t$0\n}", "foreach loop"),
    (S, "for", "for (${1:\\$i} = 0; $1 < ${2:\\$n}; $1++) {\n\t$0\n}", "for loop"),
    (S, "while", "while (${1:condition}) {\n\t$0\n}", "while loop"),
    (S, "try", "try {\n\t$1\n} catch (${2:Exception} \\$e) {\n\t$0\n}", "try/catch"),
]

HTML_SNIPPETS: list[Snippet] = [(
    S,
    "html5",
    "<!DOCTYPE html>\n<html lang=\"${1:en}\">\n<head>\n\t<meta charset=\"UTF-8\">\n\t<meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\n\t<title>${2:Document}</title>\n</head>\n<body>\n\t$0\n</body>\n</html>",
    "HTML5 document",
)]

MARKDOWN_SNIPPETS: list[Snippet] = [
    (S, "codeblock", "```${1:language}\n$0\n```", "Fenced code block"),
    (S, "link", "[${1:text}](${2:url})", "Link"),
    (S, "image", "![${1:alt}](${2:url})", "Image"),
    (S, "table", "| ${1:Column} | ${2:Column} |\n| --- | --- |\n| $3 | $0 |", "Table"),
]

LANGS: list[Lang] = [
    Lang(ids=["python"],
        triggers=["."],
        members_after=["."],
        keywords="False None True and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield match case",
        functions="abs aiter all anext any ascii bin breakpoint callable chr compile delattr dir divmod enumerate eval exec filter format getattr globals hasattr hash help hex id input isinstance issubclass iter len locals map max min next oct open ord pow print repr reversed round setattr sorted sum super vars zip __import__",
        types="bool bytearray bytes complex dict float frozenset int list memoryview object property range set slice staticmethod classmethod str tuple type Exception BaseException ValueError TypeError KeyError IndexError AttributeError RuntimeError NotImplementedError StopIteration OSError FileNotFoundError ImportError ZeroDivisionError AssertionError KeyboardInterrupt PermissionError TimeoutError",
        constants="NotImplemented Ellipsis __name__ __file__ __doc__",
        variables="self cls",
        modules="os sys re json math time datetime pathlib subprocess collections itertools functools typing dataclasses asyncio logging random shutil argparse unittest",
        members="append extend insert pop remove clear copy count index sort reverse keys values items get update setdefault join split rsplit strip lstrip rstrip replace format startswith endswith lower upper title find encode decode splitlines read write readlines close add discard union intersection difference",
        comments=["#"],
        snippets=[PYTHON_SNIPPETS]),
    Lang(ids=["rust"],
        triggers=[".", "::"],
        members_after=[".", "::"],
        keywords="as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while",
        types="i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 bool char str String Vec Option Result Box Rc Arc RefCell Cell HashMap HashSet BTreeMap BTreeSet VecDeque Path PathBuf Mutex",
        constants="Some None Ok Err",
        modules="std core alloc",
        members="unwrap expect unwrap_or unwrap_or_default unwrap_or_else map map_err and_then ok ok_or is_some is_none is_ok is_err as_ref as_mut as_str as_bytes clone to_string to_owned into iter iter_mut into_iter collect filter filter_map for_each enumerate zip rev take skip chain fold any all find position count sum max min len is_empty push push_str pop insert remove get get_mut contains extend sort sort_by dedup retain join split trim lines chars bytes starts_with ends_with replace parse borrow borrow_mut lock read write",
        paths="new default from with_capacity from_str from_utf8 into_iter iter collections fmt fs io env path process sync thread time",
        comments=["//"],
        snippets=[RUST_SNIPPETS]),
    Lang(ids=["javascript"],
        extra="$",
        triggers=["."],
        members_after=["."],
        keywords=JS_KEYWORDS,
        functions=JS_FUNCTIONS,
        types=JS_GLOBALS,
        variables="console document window globalThis process module exports navigator localStorage",
        members=JS_MEMBERS,
        comments=["//"],
        snippets=[JS_SNIPPETS]),
    Lang(ids=["typescript", "tsx"],
        extra="$",
        triggers=["."],
        members_after=["."],
        keywords=JS_KEYWORDS + " interface type enum implements private protected public readonly abstract declare namespace keyof as is any unknown never string number boolean symbol bigint object satisfies infer",
        functions=JS_FUNCTIONS,
        types=JS_GLOBALS + " Record Partial Required Readonly Pick Omit Exclude Extract ReturnType Parameters Awaited NonNullable",
        variables="console document window globalThis process module exports navigator localStorage",
        members=JS_MEMBERS,
        comments=["//"],
        snippets=[JS_SNIPPETS, TS_SNIPPETS]),
    Lang(ids=["java"],
        triggers=["."],
        members_after=["."],
        keywords="abstract assert boolean break byte case catch char class const continue default do double else enum extends final finally float for goto if implements import instanceof int interface long native new package private protected public return short static strictfp super switch synchronized this throw throws transient try void volatile while var record sealed permits yield true false null",
        types="String Object Integer Long Double Float Boolean Character Byte Short Math System List ArrayList LinkedList Map HashMap TreeMap Set HashSet Optional StringBuilder Exception RuntimeException IllegalArgumentException IllegalStateException IOException Thread Runnable Arrays Collections Stream Collectors Iterator Scanner",
        members="println printf print length size get put add remove contains containsKey isEmpty equals hashCode toString substring charAt indexOf split trim stream map filter collect forEach append toList of format valueOf parseInt",
        comments=["//"],
        snippets=[JAVA_SNIPPETS]),
    Lang(ids=["c"],
        extra="#",
        triggers=[".", "->"],
        members_after=[".", "->"],
        keywords=C_KEYWORDS,
        functions=C_FUNCTIONS,
        types=C_TYPES,
        constants="NULL EOF stdin stdout stderr EXIT_SUCCESS EXIT_FAILURE",
        comments=["//"],
        snippets=[C_SNIPPETS, C_COMMON_SNIPPETS]),
    Lang(ids=["cpp"],
        extra="#",
        triggers=[".", "->", "::"],
        members_after=[".", "->", "::"],
        keywords=C_KEYWORDS + " alignas alignof catch class concept consteval constexpr constinit const_cast co_await co_return co_yield decltype delete dynamic_cast explicit export final friend mutable namespace new noexcept nullptr operator override private protected public reinterpret_cast requires static_assert static_cast template this thread_local throw try typeid typename using virtual",
        functions=C_FUNCTIONS + " make_unique make_shared move forward swap sort find begin end getline to_string",
        types=C_TYPES + " std string vector map unordered_map set unordered_set pair tuple optional variant unique_ptr shared_ptr weak_ptr array deque list queue stack function string_view",
        constants="NULL EOF cout cin cerr endl",
        members="push_back emplace_back pop_back size empty clear begin end insert erase find at front back data c_str length substr reserve resize count get reset first second",
        paths="cout cin cerr endl move forward swap sort find begin end make_unique make_shared to_string getline",
        comments=["//"],
        snippets=[CPP_SNIPPETS, C_COMMON_SNIPPETS]),
    Lang(ids=["go"],
        triggers=["."],
        members_after=["."],
        keywords="break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var",
        functions="append cap clear close complex copy delete imag len make max min new panic print println real recover",
        types="bool byte complex64 complex128 error float32 float64 int int8 int16 int32 int64 rune string uint uint8 uint16 uint32 uint64 uintptr any comparable",
        constants="true false nil iota",
        modules="fmt os strings strconv errors io bufio time sync context http json sort math bytes filepath",
        members="Println Printf Sprintf Sprint Errorf Fprintf Fprintln Contains Split Join TrimSpace HasPrefix HasSuffix ReplaceAll Itoa Atoi New Is As ReadFile WriteFile Open Create Exit Getenv Now Since Sleep Lock Unlock Add Done Wait Background WithCancel Marshal Unmarshal Slice Strings Ints",
        comments=["//"],
        snippets=[GO_SNIPPETS]),
    Lang(ids=["bash"],
        triggers=["$", "${"],
        keywords="if then else elif fi case esac for select while until do done in function time return break continue exit local export readonly declare unset shift source alias set trap eval exec",
        functions="echo printf read cd pwd ls cat grep sed awk find xargs sort uniq head tail wc cut tr mkdir rm cp mv touch chmod chown ln test tee curl wget git tar sudo kill ps jq sleep date basename dirname realpath mktemp which command type",
        variables="HOME PATH PWD USER SHELL IFS RANDOM LINENO BASH_SOURCE FUNCNAME OLDPWD HOSTNAME EDITOR",
        comments=["#"],
        snippets=[BASH_SNIPPETS]),
    Lang(ids=["sql"],
        triggers=["."],
        members_after=["."],
        ignore_case=True,
        keywords="SELECT FROM WHERE AND OR NOT INSERT INTO VALUES UPDATE SET DELETE CREATE TABLE ALTER ADD COLUMN DROP INDEX VIEW JOIN INNER LEFT RIGHT FULL OUTER CROSS ON AS GROUP BY ORDER HAVING LIMIT OFFSET DISTINCT UNION ALL EXISTS IN BETWEEN LIKE IS NULL PRIMARY KEY FOREIGN REFERENCES DEFAULT UNIQUE CHECK CONSTRAINT CASE WHEN THEN ELSE END ASC DESC WITH RETURNING BEGIN COMMIT ROLLBACK TRANSACTION IF REPLACE TRUNCATE CASCADE",
        functions="COUNT SUM AVG MIN MAX COALESCE NULLIF CAST ROUND LENGTH LOWER UPPER SUBSTRING TRIM NOW CONCAT ABS IFNULL ROW_NUMBER RANK STRFTIME",
        types="INTEGER INT BIGINT SMALLINT TEXT VARCHAR CHAR BOOLEAN REAL FLOAT DOUBLE DECIMAL NUMERIC DATE TIME TIMESTAMP BLOB JSON SERIAL UUID",
        constants="TRUE FALSE CURRENT_TIMESTAMP CURRENT_DATE",
        comments=["--"],
        snippets=[SQL_SNIPPETS]),
    Lang(ids=["ruby"],
        extra="@",
        triggers=[".", "::"],
        members_after=[".", "::"],
        keywords="BEGIN END alias and begin break case class def defined? do else elsif end ensure false for if in module next nil not or redo rescue retry return self super then true undef unless until when while yield",
        functions="require require_relative attr_accessor attr_reader attr_writer include extend private protected public raise puts print p pp lambda proc loop format sleep",
        types="Array Hash String Integer Float Symbol Proc Struct Object Kernel Comparable Enumerable StandardError RuntimeError ArgumentError File Dir Time IO",
        members="each map select reject reduce each_with_index each_with_object find include? empty? nil? length size first last push pop join split strip gsub sub upcase downcase to_s to_i to_f to_sym to_a keys values merge sort sort_by uniq flatten compact freeze new",
        paths="new",
        comments=["#"],
        snippets=[RUBY_SNIPPETS]),
    Lang(ids=["lua"],
        triggers=[".", ":"],
        members_after=[".", ":"],
        keywords="and break do else elseif end false for function goto if in local nil not or repeat return then true until while",
        functions="print type pairs ipairs tostring tonumber require pcall xpcall error assert select setmetatable getmetatable rawget rawset rawequal next unpack load dofile collectgarbage",
        modules="string table math os io coroutine utf8 debug package",
        members="format sub gsub find match gmatch len upper lower rep byte char insert remove concat sort unpack floor ceil max min abs random sqrt huge pi time clock date getenv execute open read write lines close",
        comments=["--"],
        snippets=[LUA_SNIPPETS]),
    Lang(ids=["php"],
        extra="$",
        triggers=["->", "::", "$"],
        members_after=["->", "::"],
        keywords="abstract and array as break callable case catch class clone const continue declare default do echo else elseif empty enddeclare endfor endforeach endif endswitch endwhile enum extends final finally fn for foreach function global goto if implements include include_once instanceof insteadof interface isset list match namespace new or print private protected public readonly require require_once return static switch throw trait try unset use var while xor yield true false null self parent",
        functions="strlen str_replace str_contains str_starts_with substr strpos explode implode trim sprintf printf count array_map array_filter array_keys array_values array_merge array_push in_array array_key_exists json_encode json_decode var_dump print_r is_array is_string is_int is_null intval strval file_get_contents file_put_contents date time header htmlspecialchars preg_match preg_replace ucfirst strtolower strtoupper",
        variables="$this $_GET $_POST $_SERVER $_SESSION $_COOKIE $_FILES $_ENV $GLOBALS",
        paths="class",
        comments=["//", "#"],
        snippets=[PHP_SNIPPETS]),
    Lang(ids=["html"], extra="-", triggers=["<", "</"], in_strings=True, snippets=[HTML_SNIPPETS]),
    Lang(ids=["css"], extra="-@", triggers=[":"], in_strings=True),
    Lang(ids=["xml"], extra="-", triggers=["<", "</"], in_strings=True),
    Lang(ids=["json"], in_strings=True, constants="true false null"),
    Lang(ids=["yaml"], extra="-", in_strings=True, constants="true false null", comments=["#"]),
    Lang(ids=["toml"], extra="-", in_strings=True, constants="true false", comments=["#"]),
    Lang(ids=["markdown"], auto=False, in_strings=True, snippets=[MARKDOWN_SNIPPETS]),
]


def lang(language: str | None) -> Lang:
    for entry in LANGS:
        if language in entry.ids:
            return entry
    return EMPTY


# -- words -----------------------------------------------------------------------------------

def is_word(c: str, extra: str) -> bool:
    return c.isalnum() or c == "_" or (c != "" and c in extra)


def word_extra(language: str | None) -> str:
    return lang(language).extra


def word_start(line: str, col: int, extra: str) -> int:
    """Where the word that ends at `col` starts."""
    start = min(col, len(line))
    while start > 0 and is_word(line[start - 1], extra):
        start -= 1
    return start


def is_trigger(language: str | None, before: str) -> bool:
    """Does the text left of the cursor open the widget by itself (`x.`, `p->`, `<`...)?"""
    entry = lang(language)
    for trigger in entry.triggers:
        if not before.endswith(trigger):
            continue
        if trigger == ".":
            # `x.` and `f().` are member access; `1.` and `...` are not.
            rest = before[:-1]
            token = rest[word_start(rest, len(rest), entry.extra):]
            previous = rest[-1:]
            return (previous in (")", "]") or (previous != "" and is_word(previous, entry.extra))) and not token[:1].isdigit()
        return True
    return False


def auto_suggest(language: str | None) -> bool:
    return lang(language).auto


def ignore_case(language: str | None) -> bool:
    return lang(language).ignore_case


def in_line_comment(markers: list[str], before: str) -> bool:
    """Is the end of `before` inside a line comment? (Doesn't wait for the highlighter.)"""
    quote = None
    escaped = False
    for i, c in enumerate(before):
        if quote:
            if escaped:
                escaped = False
            elif c == "\\":
                escaped = True
            elif c == quote:
                quote = None
        elif c in "\"'`":
            quote = c
        elif any(before.startswith(m, i) for m in markers):
            return True
    return False


def quiet_at(language: str | None, line: str, col: int, highlights: list) -> bool:
    """True inside a comment, or a string in a language where those are just text.

    `highlights` are TextArea's (start byte, end byte, capture name) for the line.
    Looks at the character before `col`: the newest one may not be highlighted yet."""
    if col <= 0:
        return False
    entry = lang(language)
    if in_line_comment(entry.comments, line[:col]):
        return True
    byte = len(line[:col - 1].encode("utf-8"))
    for start, end, name in highlights:
        if start <= byte and (end is None or byte < end):
            if name.startswith("comment") or (not entry.in_strings and name.startswith("string")):
                return True
    return False


FUNCTION_DECLARATIONS = {"def", "fn", "func", "function", "sub"}
TYPE_DECLARATIONS = {"class", "struct", "enum", "interface", "trait", "type", "union", "module", "namespace", "record", "impl"}
SEEN, CAPITALISED, USED, DECLARED = range(4)


def _scan(lines: list[str], rows: range, extra: str, skip: tuple[int, int] | None, out: dict[str, tuple[Kind, int]]) -> None:
    """Collect the words in `lines` with a guess at what each one is."""
    word_re = re.compile(r"[\w" + re.escape(extra) + r"]+") if extra else re.compile(r"\w+")
    for row in rows:
        line = lines[row]
        previous = None
        for match in word_re.finditer(line):
            word, start, end = match.group(), match.start(), match.end()
            declared_by, previous = previous, word
            if (row, start) == skip or word[0].isdigit() or len(word) < 2 or len(out) >= MAX_WORDS:
                continue
            if not any(c.isalnum() for c in word):
                continue
            before = line[:start].rstrip()[-1:]
            after = line[end:].lstrip()[:1]
            member = before == "." or line[:start].endswith("->")
            if declared_by in FUNCTION_DECLARATIONS:
                kind, evidence = Kind.FUNCTION, DECLARED
            elif declared_by in TYPE_DECLARATIONS:
                kind, evidence = Kind.CLASS, DECLARED
            elif after == "(":
                kind, evidence = (Kind.METHOD if member else Kind.FUNCTION), USED
            elif member:
                kind, evidence = Kind.FIELD, USED
            elif not any(c.islower() for c in word) and any(c.isupper() for c in word):
                kind, evidence = Kind.CONSTANT, CAPITALISED
            elif word[0].isupper():
                kind, evidence = Kind.CLASS, CAPITALISED
            else:
                kind, evidence = Kind.VARIABLE, SEEN
            known = out.get(word)
            if known is None or known[1] < evidence:
                out[word] = (kind, evidence)


# -- building the list -------------------------------------------------------------------------

@dataclass
class Context:
    """Where the widget is being opened."""

    language: str | None
    lines: list[str]
    row: int
    start: int  # start of the word being completed
    others: list[list[str]] = field(default_factory=list)  # other open documents, same language


class _Out:
    def __init__(self, ignore_case: bool) -> None:
        self.items: list[Item] = []
        self.seen: set[str] = set()
        self.ignore_case = ignore_case
        self.group = 0

    def key(self, label: str) -> str:
        return label.lower() if self.ignore_case else label

    def add(self, item: Item) -> None:
        item.group = self.group
        # Snippets may share a label with a keyword ("for"); VS Code lists both.
        key = ("\1" if item.kind is Kind.SNIPPET else "") + self.key(item.label)
        if key not in self.seen:
            self.seen.add(key)
            self.items.append(item)

    def list(self, words: str, kind: Kind, detail: str) -> None:
        for word in words.split():
            self.add(Item(word, kind, detail))
        self.group += 1

    def has(self, label: str) -> bool:
        return self.key(label) in self.seen


def candidates(cx: Context) -> list[Item]:
    """Everything that could be suggested at this spot (filtered later as you type)."""
    entry = lang(cx.language)
    before = cx.lines[cx.row][:cx.start]
    out = _Out(entry.ignore_case)

    if cx.language == "html" and _html(before, out):
        return out.items
    if cx.language == "xml" and (before.endswith("<") or before.endswith("</")):
        closing = before.endswith("</")
        tags: set[str] = set()
        for text in [*cx.lines, *(line for lines in cx.others for line in lines)]:
            tags.update(re.findall(r"<([\w\-:.]+)", text))
        for tag in sorted(tags):
            out.add(Item(tag, Kind.PROPERTY, "", f"{tag}>" if closing else f"{tag}>$0</{tag}>", builtin=False))
        return out.items
    if cx.language == "css":
        _css(cx, before, out)

    member = any(before.endswith(t) for t in entry.members_after)
    if cx.language == "bash" and (before.endswith("$") or before.endswith("${")):
        out.list(entry.variables, Kind.VARIABLE, "variable")
    elif before.endswith("::"):
        out.list(entry.paths, Kind.FUNCTION, "associated item")
        out.list(entry.types, Kind.CLASS, "type")
        out.list(entry.constants, Kind.CONSTANT, "constant")
    elif member:
        out.list(entry.members, Kind.METHOD, "method")
    elif cx.language != "css":
        out.list(entry.keywords, Kind.KEYWORD, "keyword")
        out.list(entry.functions, Kind.FUNCTION, "built-in function")
        out.list(entry.types, Kind.CLASS, "built-in type")
        out.list(entry.constants, Kind.CONSTANT, "constant")
        out.list(entry.modules, Kind.MODULE, "module")
        out.list(entry.variables, Kind.VARIABLE, "variable")
        for group in entry.snippets:
            for kind, label, body, detail in group:
                out.add(Item(label, kind, detail, body))
        if cx.language == "html":
            # Emmet-style: a bare tag name expands to the element.
            void = HTML_VOID.split()
            for tag in HTML_TAGS.split():
                out.add(Item(tag, Kind.SNIPPET, "Emmet Abbreviation", f"<{tag}>" if tag in void else f"<{tag}>$0</{tag}>"))

    # Words from the open documents of this language.
    out.group += 1
    found: dict[str, tuple[Kind, int]] = {}
    rows = range(max(0, cx.row - SCAN_LINES), min(len(cx.lines), cx.row + SCAN_LINES))
    _scan(cx.lines, rows, entry.extra, (cx.row, cx.start), found)
    for lines in cx.others:
        _scan(lines, range(min(len(lines), SCAN_LINES)), entry.extra, None, found)
    text_only = cx.language in (None, "markdown")
    for word in sorted(found):
        if not out.has(word):
            out.add(Item(word, Kind.TEXT if text_only else found[word][0], builtin=False))
    return out.items


def _html(before: str, out: _Out) -> bool:
    """Tags after `<`, attributes inside a tag. True when that is all there is to offer."""
    void = HTML_VOID.split()
    if before.endswith("</"):
        for tag in HTML_TAGS.split():
            if tag not in void:
                out.add(Item(tag, Kind.PROPERTY, "closing tag", f"{tag}>"))
        return True
    if before.endswith("<"):
        for tag in HTML_TAGS.split():
            out.add(Item(tag, Kind.PROPERTY, "element", f"{tag}>" if tag in void else f"{tag}>$0</{tag}>"))
        return True
    # Inside an opening tag, outside any quoted value: attributes.
    open_at, close_at = before.rfind("<"), before.rfind(">")
    if open_at >= 0 and close_at < open_at:
        inside = before[open_at:]
        quotes = inside.count('"') + inside.count("'")
        if any(c.isspace() for c in inside) and not inside.startswith("</") and quotes % 2 == 0:
            for attribute in HTML_ATTRIBUTES.split():
                body = f'{attribute}$1="$0"' if attribute.endswith("-") else f'{attribute}="$0"'
                out.add(Item(attribute, Kind.VALUE, "attribute", body))
            out.list(HTML_BOOLEAN_ATTRIBUTES, Kind.VALUE, "attribute")
            return True
    return False


def _css(cx: Context, before: str, out: _Out) -> None:
    """Selectors and at-rules outside a block; properties, then values, inside one."""
    depth = sum(t.count("{") - t.count("}") for t in [*cx.lines[:cx.row], before])
    statement = re.split(r"[;{}]", before)[-1]
    if depth <= 0:
        if before.endswith(":"):
            out.list(CSS_PSEUDO, Kind.KEYWORD, "pseudo-class")
        else:
            out.list(CSS_AT_RULES, Kind.KEYWORD, "at-rule")
            out.list(HTML_TAGS, Kind.PROPERTY, "element")
    elif ":" in statement:
        out.list(CSS_VALUES, Kind.VALUE, "value")
        for function in CSS_FUNCTIONS.split():
            out.add(Item(function, Kind.FUNCTION, "function", f"{function}($0)"))
        out.add(Item("!important", Kind.KEYWORD, "keyword"))
    else:
        for prop in CSS_PROPERTIES.split():
            out.add(Item(prop, Kind.PROPERTY, "property", f"{prop}: $0;"))


# -- snippets ------------------------------------------------------------------------------------

_STOP = re.compile(r"\\(.)|\$(\d+)|\$\{(\d+)(?::((?:\\.|[^}])*))?\}|(\t)|(\n)", re.S)


def expand_snippet(body: str, indent: str) -> tuple[str, tuple[int, int] | None]:
    """Expand a snippet body. A tab becomes four spaces, new lines keep `indent`, and the
    result says which stretch to select first (the lowest tab stop, else $0), in
    characters from the start of the text."""
    text: list[str] = []
    length = 0
    stops: dict[int, tuple[int, int]] = {}
    defaults: dict[int, str] = {}
    position = 0

    def push(s: str) -> None:
        nonlocal length
        text.append(s)
        length += len(s)

    for match in _STOP.finditer(body):
        push(body[position:match.start()])
        position = match.end()
        escaped, bare, numbered, default, tab, newline = match.groups()
        if escaped is not None:
            push(escaped)
        elif bare is not None or numbered is not None:
            number = int(bare if bare is not None else numbered)
            if numbered is not None:
                value = re.sub(r"\\(.)", r"\1", default or "")
                defaults[number] = value
            else:
                value = defaults.get(number, "")  # a repeated $1 mirrors its default
            at = length
            push(value)
            stops.setdefault(number, (at, length))
        elif tab:
            push("    ")
        elif newline:
            push("\n" + indent)
    push(body[position:])
    numbers = sorted(n for n in stops if n > 0)
    first = stops[numbers[0]] if numbers else stops.get(0)
    return "".join(text), first


def insert_text(item: Item, typed: str, indent: str, ignore_case: bool) -> tuple[str, tuple[int, int] | None]:
    """What to insert for an item, given the text typed so far."""
    # SQL: `sel` + Enter gives `select`, `SEL` gives `SELECT`.
    lower = ignore_case and item.builtin and any(c.isalpha() for c in typed) and not any(c.isupper() for c in typed)
    fold = (lambda s: s.lower()) if lower else (lambda s: s)
    if item.body is not None:
        return expand_snippet(fold(item.body), indent)
    return fold(item.label), None


# -- matching ------------------------------------------------------------------------------------

def fuzzy(query: str, text: str) -> tuple[int, list[int]] | None:
    """Every character of `query` in `text`, in order (case-insensitive): score and positions.
    The same scoring as the Rust build's util::fuzzy."""
    wanted = [c.lower() for c in query if not c.isspace()]
    if not wanted:
        return 0, []
    lower = text.lower() if len(text.lower()) == len(text) else "".join(c.lower()[0] for c in text)
    positions: list[int] = []
    score = 0
    start = 0
    previous = None
    for c in wanted:
        found = lower.find(c, start)
        if found < 0:
            return None
        boundary = found == 0 or not text[found - 1].isalnum() or (text[found].isupper() and text[found - 1].islower())
        score += 10 + (8 if boundary else 0) + (12 if previous is not None and previous + 1 == found else 0)
        positions.append(found)
        previous = found
        start = found + 1
    score -= len(text) // 8
    return max(score, 1), positions


class Suggest:
    """The list behind the suggest widget, open under the cursor of one editor."""

    def __init__(self, row: int, start: int, explicit: bool, items: list[Item], ignore_case: bool = False) -> None:
        self.row = row
        self.start = start  # column where the completed word starts
        self.explicit = explicit  # opened by a trigger character or Ctrl+Space
        self.items = items
        self.ignore_case = ignore_case
        self.shown: list[tuple[Item, list[int]]] = []
        self.selected = 0
        self.top = 0

    def filter(self, typed: str) -> bool:
        """Narrow the list to what matches `typed`. False when nothing is worth showing."""
        scored = []
        same = (lambda a, b: a.lower() == b.lower()) if self.ignore_case else (lambda a, b: a == b)
        for index, item in enumerate(self.items):
            if not typed:
                scored.append((0, index, item, []))
                continue
            match = fuzzy(typed, item.label)
            if match is None:
                continue
            score, positions = match
            label, p = item.label, positions[0]
            # The first letter typed has to start the label or one of its parts (as in VS Code).
            if not (p == 0 or not label[p - 1].isalnum() or (label[p].isupper() and label[p - 1].islower())):
                continue
            if same(label, typed):
                score += 60
            elif label.startswith(typed) or (self.ignore_case and label.lower().startswith(typed.lower())):
                score += 30
            elif label.lower().startswith(typed.lower()):
                score += 15
            scored.append((score, index, item, positions))
        scored.sort(key=lambda s: (-s[0], s[2].group if not typed else 0, s[2].label.lower(), s[1]))
        self.shown = [(item, positions) for _, _, item, positions in scored[:500]]
        self.selected = 0
        self.top = 0
        # A word typed out in full, with nothing else to offer, needs no popup (and must
        # not swallow the Enter that follows it).
        if len(self.shown) == 1:
            item = self.shown[0][0]
            if item.body is None and same(item.label, typed):
                return False
        return bool(self.shown)

    @property
    def current(self) -> Item | None:
        return self.shown[self.selected][0] if self.shown else None

    @property
    def labels(self) -> list[str]:
        return [item.label for item, _ in self.shown]

    @property
    def visible(self) -> int:
        return min(len(self.shown), VISIBLE)

    def step(self, delta: int) -> None:
        """Move the highlight; single steps wrap around at either end like VS Code."""
        count = len(self.shown)
        if not count:
            return
        target = self.selected + delta
        self.selected = max(0, min(count - 1, target)) if abs(delta) > 1 else target % count
        self.keep_visible()

    def scroll(self, delta: int) -> None:
        self.top = max(0, min(len(self.shown) - self.visible, self.top + delta))
        self.selected = max(self.top, min(self.top + self.visible - 1, self.selected))

    def keep_visible(self) -> None:
        if self.selected < self.top:
            self.top = self.selected
        elif self.selected >= self.top + self.visible:
            self.top = self.selected + 1 - self.visible
