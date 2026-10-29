import re
import urllib.parse
from html.parser import HTMLParser


FORMATS = ("markdown", "html", "text")
BOLD = ("b",)
ITALIC = ("i",)
UNDERLINE = ("u",)
STRIKE = ("s",)
SPOILER = ("tg-spoiler",)
CODE = ("code",)
STYLE_MARKS = {"b", "i", "u", "s", "tg-spoiler"}
ASCII_PUNCTUATION = set("!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~")
FENCE_RE = re.compile(r"^ {0,3}(`{3,}|~{3,})[ \t]*([A-Za-z0-9_+#.-]{0,32})[^`]*$")
HEADING_RE = re.compile(r"^ {0,3}#{1,6}[ \t]+(.*?)(?:[ \t]+#+)?[ \t]*$")
RULE_RE = re.compile(r"^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$")
QUOTE_RE = re.compile(r"^ {0,3}>[ ]?(.*)$")
BULLET_RE = re.compile(r"^([ \t]*)[-*+][ \t]+(.*)$")
ORDERED_RE = re.compile(r"^([ \t]*)([0-9]{1,9})([.)])[ \t]+(.*)$")
TASK_RE = re.compile(r"^\[([ xX])\][ \t]+(.*)$")
TABLE_SEPARATOR_RE = re.compile(r"^[ \t]*\|?[ \t]*:?-{3,}:?[ \t]*(?:\|[ \t]*:?-{3,}:?[ \t]*)*\|?[ \t]*$")
AUTOLINK_RE = re.compile(r"<((?:https?|tg)://[^\s<>]+|mailto:[^\s<>]+)>")
INLINE_TOKENS = (("***", (BOLD, ITALIC)), ("**", (BOLD,)), ("__", (BOLD,)), ("~~", (STRIKE,)), ("||", (SPOILER,)), ("*", (ITALIC,)), ("_", (ITALIC,)))
HTML_STYLE_TAGS = {"b": "b", "strong": "b", "i": "i", "em": "i", "u": "u", "ins": "u", "s": "s", "strike": "s", "del": "s", "tg-spoiler": "tg-spoiler"}


def utf16_len(text):
    return len(text.encode("utf-16-le")) // 2


def safe_link_url(url):
    if not isinstance(url, str) or not url or url.strip() != url or len(url) > 2048 or any(ord(character) < 33 or ord(character) == 127 for character in url):
        return None
    scheme = url.split(":", 1)[0].lower() if ":" in url else ""
    if scheme in {"http", "https"}:
        try:
            parsed = urllib.parse.urlsplit(url)
        except ValueError:
            return None
        return url if parsed.netloc and parsed.hostname else None
    if scheme == "tg" and url.lower().startswith("tg://") and len(url) > 5:
        return url
    if scheme == "mailto" and "@" in url:
        return url
    return None


def escape(text):
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def normalize_marks(marks):
    pre = [mark for mark in marks if mark[0] == "pre"]
    if pre:
        return (pre[0],)
    quote = [mark for mark in marks if mark[0] == "blockquote"][:1]
    link = [mark for mark in marks if mark[0] == "a"][:1]
    if any(mark[0] == "code" for mark in marks) and not link:
        return tuple(quote) + (CODE,)
    styles = []
    for mark in marks:
        if mark[0] in STYLE_MARKS and mark not in styles:
            styles.append(mark)
    return tuple(quote) + tuple(link) + tuple(styles)


def open_tag(mark):
    if mark[0] == "a":
        return '<a href="' + escape(mark[1]).replace('"', "&quot;") + '">'
    if mark[0] == "pre":
        return f'<pre><code class="language-{mark[1]}">' if mark[1] else "<pre>"
    if mark[0] == "blockquote":
        return "<blockquote expandable>" if mark[1] else "<blockquote>"
    return f"<{mark[0]}>"


def close_tag(mark):
    if mark[0] == "pre":
        return "</code></pre>" if mark[1] else "</pre>"
    return f"</{mark[0]}>"


def segments_html(segments):
    output = []
    stack = ()
    for text, marks in segments:
        if not text:
            continue
        marks = normalize_marks(marks)
        common = 0
        while common < len(stack) and common < len(marks) and stack[common] == marks[common]:
            common += 1
        output.extend(close_tag(mark) for mark in reversed(stack[common:]))
        output.extend(open_tag(mark) for mark in marks[common:])
        stack = marks
        output.append(escape(text))
    output.extend(close_tag(mark) for mark in reversed(stack))
    return "".join(output)


def segments_units(segments):
    return sum(utf16_len(text) for text, _ in segments)


def can_open(text, index, token):
    after = text[index + len(token)] if index + len(token) < len(text) else ""
    before = text[index - 1] if index > 0 else ""
    if not after or after.isspace() or (len(token) == 1 and after == token):
        return False
    return not (token[0] == "_" and before and before.isalnum())


def can_close(text, index, token):
    before = text[index - 1] if index > 0 else ""
    after = text[index + len(token)] if index + len(token) < len(text) else ""
    if not before or before.isspace():
        return False
    return not (token[0] == "_" and after and after.isalnum())


def code_span_end(text, start, run):
    index = start
    while index < len(text):
        found = text.find("`" * run, index)
        if found < 0:
            return -1
        end = found + run
        if (found == 0 or text[found - 1] != "`") and (end >= len(text) or text[end] != "`"):
            return found
        while end < len(text) and text[end] == "`":
            end += 1
        index = end
    return -1


def backtick_run(text, index):
    end = index
    while end < len(text) and text[end] == "`":
        end += 1
    return end - index


def find_closer(text, start, token):
    index = start
    while index < len(text):
        character = text[index]
        if character == "\\" and index + 1 < len(text):
            index += 2
            continue
        if character == "`":
            run = backtick_run(text, index)
            end = code_span_end(text, index + run, run)
            index = end + run if end >= 0 else index + run
            continue
        if text.startswith(token, index):
            single = len(token) == 1 and text.startswith(token * 2, index)
            if single:
                index += 2
                continue
            if can_close(text, index, token):
                return index
        index += 1
    return -1


def parse_link(text, start):
    depth = 0
    index = start
    label_end = -1
    while index < len(text):
        character = text[index]
        if character == "\\":
            index += 2
            continue
        if character == "`":
            run = backtick_run(text, index)
            end = code_span_end(text, index + run, run)
            index = end + run if end >= 0 else index + run
            continue
        if character == "[":
            depth += 1
        elif character == "]":
            depth -= 1
            if depth == 0:
                label_end = index
                break
        index += 1
    if label_end < 0 or label_end + 1 >= len(text) or text[label_end + 1] != "(":
        return None
    index = label_end + 2
    nested = 0
    while index < len(text):
        character = text[index]
        if character == "\\":
            index += 2
            continue
        if character == "(":
            nested += 1
        elif character == ")":
            if nested == 0:
                destination = text[label_end + 2 : index].strip()
                title = re.match(r'^(\S+)\s+(?:"[^"]*"|\'[^\']*\')$', destination)
                if title:
                    destination = title.group(1)
                if destination.startswith("<") and destination.endswith(">"):
                    destination = destination[1:-1]
                url = safe_link_url(destination)
                return (text[start + 1 : label_end], url, index + 1) if url else None
            nested -= 1
        elif character == "\n":
            return None
        index += 1
    return None


def inline(text, marks, depth=0):
    if depth > 12:
        return [(text, marks)]
    segments = []
    buffer = []

    def flush():
        if buffer:
            segments.append(("".join(buffer), marks))
            buffer.clear()

    index = 0
    while index < len(text):
        character = text[index]
        if character == "\\" and index + 1 < len(text) and text[index + 1] in ASCII_PUNCTUATION:
            buffer.append(text[index + 1])
            index += 2
            continue
        if character == "`":
            run = backtick_run(text, index)
            end = code_span_end(text, index + run, run)
            if end >= 0:
                code = text[index + run : end]
                if len(code) > 2 and code.startswith(" ") and code.endswith(" ") and code.strip():
                    code = code[1:-1]
                flush()
                segments.append((code, marks + (CODE,)))
                index = end + run
                continue
            buffer.append("`" * run)
            index += run
            continue
        if character == "<":
            match = AUTOLINK_RE.match(text, index)
            if match and safe_link_url(match.group(1)):
                flush()
                segments.append((match.group(1), marks + (("a", match.group(1)),)))
                index = match.end()
                continue
        if character == "[" or (character == "!" and text.startswith("![", index)):
            link = parse_link(text, index + (1 if character == "!" else 0))
            if link is not None:
                label, url, end = link
                flush()
                segments.extend(inline(label or url, marks + (("a", url),), depth + 1))
                index = end
                continue
        matched = False
        for token, added in INLINE_TOKENS:
            if text.startswith(token, index) and can_open(text, index, token):
                close = find_closer(text, index + len(token), token)
                if close > index + len(token):
                    flush()
                    segments.extend(inline(text[index + len(token) : close], marks + added, depth + 1))
                    index = close + len(token)
                    matched = True
                    break
        if matched:
            continue
        buffer.append(character)
        index += 1
    flush()
    return segments


def closes_fence(line, marker):
    stripped = line.strip()
    return len(stripped) >= len(marker) and stripped == marker[0] * len(stripped)


def split_table_row(line):
    value = line.strip()
    if value.startswith("|"):
        value = value[1:]
    if value.endswith("|") and not value.endswith("\\|"):
        value = value[:-1]
    return [cell.strip().replace("\\|", "|") for cell in re.split(r"(?<!\\)\|", value)]


def plain_cell(value):
    value = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", value)
    return re.sub(r"(\*\*|__|~~|\|\||`)", "", value).strip()


def table_text(lines):
    rows = [[plain_cell(cell) for cell in split_table_row(line)] for line in lines if not TABLE_SEPARATOR_RE.match(line)]
    width = max(len(row) for row in rows)
    rows = [row + [""] * (width - len(row)) for row in rows]
    widths = [max(len(row[index]) for row in rows) for index in range(width)]
    rendered = []
    for index, row in enumerate(rows):
        rendered.append(" | ".join(cell.ljust(widths[column]) for column, cell in enumerate(row)).rstrip())
        if index == 0 and len(rows) > 1:
            rendered.append("-+-".join("-" * max(1, size) for size in widths))
    return "\n".join(rendered)


def is_table_start(lines, index):
    return index + 1 < len(lines) and "|" in lines[index] and "|" in lines[index + 1] and TABLE_SEPARATOR_RE.match(lines[index + 1]) is not None


def list_prefix(line):
    bullet = BULLET_RE.match(line)
    if bullet and not RULE_RE.match(line):
        level = min(len(bullet.group(1).expandtabs(4)) // 2, 4)
        content = bullet.group(2)
        task = TASK_RE.match(content)
        if task:
            return "  " * level + ("☑ " if task.group(1) in "xX" else "☐ "), task.group(2)
        return "  " * level + ("• " if level == 0 else "◦ "), content
    ordered = ORDERED_RE.match(line)
    if ordered:
        level = min(len(ordered.group(1).expandtabs(4)) // 2, 4)
        return "  " * level + f"{ordered.group(2)}. ", ordered.group(4)
    return None


def markdown_blocks(text):
    lines = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
    blocks = []
    paragraph = []
    state = {"gap": False}

    def add(segments):
        if segments_units(segments):
            blocks.append(("\n\n" if blocks and state["gap"] else "\n" if blocks else "", segments))
            state["gap"] = False

    def flush():
        if paragraph:
            add(inline("\n".join(line.rstrip() for line in paragraph), ()))
            paragraph.clear()

    index = 0
    while index < len(lines):
        line = lines[index]
        fence = FENCE_RE.match(line)
        if fence:
            flush()
            body = []
            index += 1
            while index < len(lines) and not closes_fence(lines[index], fence.group(1)):
                body.append(lines[index])
                index += 1
            index += 1
            code = "\n".join(body).rstrip("\n")
            add([(code, (("pre", fence.group(2)),))] if code.strip() else [])
            continue
        if is_table_start(lines, index):
            flush()
            rows = []
            while index < len(lines) and "|" in lines[index] and lines[index].strip():
                rows.append(lines[index])
                index += 1
            add([(table_text(rows), (("pre", ""),))])
            continue
        if not line.strip():
            flush()
            state["gap"] = bool(blocks)
            index += 1
            continue
        heading = HEADING_RE.match(line)
        if heading:
            flush()
            add(inline(heading.group(1), (BOLD,)))
            index += 1
            continue
        if RULE_RE.match(line):
            flush()
            add([("──────────", ())])
            index += 1
            continue
        if QUOTE_RE.match(line):
            flush()
            segments = []
            mark = ("blockquote", False)
            while index < len(lines) and QUOTE_RE.match(lines[index]):
                quoted = re.sub(r"^(?: {0,3}>[ ]?)+", "", lines[index]).rstrip()
                if segments:
                    segments.append(("\n", (mark,)))
                listed = list_prefix(quoted)
                if listed:
                    segments.append((listed[0], (mark,)))
                    quoted = listed[1]
                segments.extend(inline(quoted, (mark,)))
                index += 1
            add(segments)
            continue
        listed = list_prefix(line)
        if listed:
            flush()
            add([(listed[0], ())] + inline(listed[1].rstrip(), ()))
            index += 1
            continue
        paragraph.append(line)
        index += 1
    flush()
    return blocks


class _TelegramHtmlReader(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.segments = []
        self.stack = []

    def marks(self):
        return tuple(entry["mark"] for entry in self.stack if entry["mark"] is not None)

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if len(attributes) != len(attrs):
            raise ValueError(f"Telegram HTML <{tag}> repeats an attribute")
        mark = None
        if tag in HTML_STYLE_TAGS and not attributes:
            mark = (HTML_STYLE_TAGS[tag],)
        elif tag == "span" and attributes == {"class": "tg-spoiler"}:
            mark = SPOILER
        elif tag == "a" and set(attributes) == {"href"}:
            url = safe_link_url(attributes["href"])
            if url is None:
                raise ValueError("Telegram HTML links must use http, https, tg or mailto URLs")
            mark = ("a", url)
        elif tag == "code" and self.stack and self.stack[-1]["tag"] == "pre" and not self.stack[-1]["has_code"] and set(attributes) <= {"class"}:
            language = attributes.get("class") or ""
            if language and not re.fullmatch(r"language-[A-Za-z0-9_+#.-]{1,32}", language):
                raise ValueError("Telegram HTML code language must be class=\"language-NAME\"")
            self.stack[-1]["has_code"] = True
            self.stack[-1]["mark"] = ("pre", language[len("language-") :] if language else "")
        elif tag == "code" and not attributes:
            mark = CODE
        elif tag == "pre" and not attributes:
            mark = ("pre", "")
        elif tag == "blockquote" and set(attributes) <= {"expandable"}:
            if any(entry["tag"] == "blockquote" for entry in self.stack):
                raise ValueError("Telegram HTML blockquotes cannot be nested")
            mark = ("blockquote", "expandable" in attributes)
        else:
            raise ValueError(f"Telegram HTML does not support <{tag}> with these attributes; use b, i, u, s, tg-spoiler, a href, code, pre or blockquote")
        self.stack.append({"tag": tag, "mark": mark, "has_code": False})

    def handle_startendtag(self, tag, attrs):
        raise ValueError(f"Telegram HTML does not support <{tag}/>")

    def handle_endtag(self, tag):
        if not self.stack or self.stack[-1]["tag"] != tag:
            raise ValueError(f"Telegram HTML closes <{tag}> that is not open")
        self.stack.pop()

    def handle_data(self, data):
        if data:
            self.segments.append((data, self.marks()))

    def handle_comment(self, data):
        raise ValueError("Telegram HTML does not support comments")

    def handle_decl(self, decl):
        raise ValueError("Telegram HTML does not support declarations")

    def handle_pi(self, data):
        raise ValueError("Telegram HTML does not support processing instructions")

    def unknown_decl(self, data):
        raise ValueError("Telegram HTML does not support declarations")


def html_segments(text):
    reader = _TelegramHtmlReader()
    reader.feed(text)
    reader.close()
    if reader.stack:
        raise ValueError(f"Telegram HTML leaves <{reader.stack[-1]['tag']}> open")
    return reader.segments


def cut_index(text, room):
    if room <= 0:
        return 0
    limit = 0
    units = 0
    for index, character in enumerate(text):
        width = 2 if ord(character) > 0xFFFF else 1
        if units + width > room:
            break
        units += width
        limit = index + 1
    if limit >= len(text):
        return limit
    for separator in ("\n", " "):
        found = text.rfind(separator, 0, limit + 1)
        if found > 0 and found >= limit // 3:
            return found
    return limit


def split_block(segments, limit):
    pieces = []
    current = []
    used = 0
    for text, marks in segments:
        while text:
            room = limit - used
            if utf16_len(text) <= room:
                current.append((text, marks))
                used += utf16_len(text)
                break
            cut = cut_index(text, room)
            if cut == 0 and current:
                pieces.append(current)
                current, used = [], 0
                continue
            if cut == 0:
                cut = 1
            head = text[:cut]
            current.append((head, marks))
            pieces.append(current)
            current, used = [], 0
            text = text[cut:]
            if text[:1] in {"\n", " "}:
                text = text[1:]
    if current:
        pieces.append(current)
    return pieces


def pack(blocks, limit):
    chunks = []
    current = []
    used = 0
    for separator, segments in blocks:
        size = segments_units(segments)
        if not size:
            continue
        joint = separator if current else ""
        if used + utf16_len(joint) + size <= limit:
            if joint:
                current.append((joint, ()))
            current.extend(segments)
            used += utf16_len(joint) + size
            continue
        if current:
            chunks.append(current)
            current, used = [], 0
        if size <= limit:
            current, used = list(segments), size
            continue
        pieces = split_block(segments, limit)
        chunks.extend(pieces[:-1])
        current, used = pieces[-1], segments_units(pieces[-1])
    if current:
        chunks.append(current)
    return [chunk for chunk in chunks if segments_units(chunk)]


def render_chunks(text, text_format, limit):
    if text_format not in FORMATS:
        raise ValueError(f"Telegram text format must be one of {', '.join(FORMATS)}")
    if not isinstance(text, str):
        raise ValueError("Telegram text must be a string")
    if text_format == "markdown":
        blocks = markdown_blocks(text)
    elif text_format == "html":
        blocks = [("", html_segments(text))]
    else:
        blocks = [("", [(text.strip("\n"), ())])]
    packed = pack(blocks, limit)
    if not any("".join(text for text, _ in chunk).strip() for chunk in packed):
        raise ValueError("Telegram message text is empty after formatting")
    return [segments_html(chunk) for chunk in packed]
