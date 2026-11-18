"""Section lookup from Word outlines and conservative printed PDF headings."""
from collections import Counter
from statistics import median
import re


CHAPTER = re.compile(r"(?:chapter|part)\s+(?:[0-9]+|[IVXLCDM]+)\b", re.I)
EXPERIMENT = re.compile(r"(?:experiment|study)\s+\d+\s*:?$", re.I)
NUMBERED = re.compile(r"^(\d{1,2}(?:\.\d{1,2})*)[.)]?\s+(?=[A-Z])")
PAGE_NUMBER = re.compile(r"(?:page\s+)?\d+(?:\s*(?:/|of)\s*\d+)?", re.I)
PAPER_ROOT = {"abstract", "introduction", "general discussion", "conclusion", "conclusions",
              "references", "bibliography", "supporting information", "acknowledgments", "author contributions"}
EXPERIMENT_CHILD = {"method", "methods", "materials and methods", "participants", "results", "discussion"}
TOC = {"contents", "table of contents"}


def normalized(value):
    return " ".join(value.casefold().split())


def height(line):
    box = line["bbox"]
    return box[3] - box[1]


def margin_key(page, line):
    """Recurring text near the same physical margin is a running header/footer."""
    x, y, _, bottom = line["bbox"]
    if not page.get("width") or not page.get("height"):
        return None
    if y <= page["height"] * .07 or bottom >= page["height"] * .93:
        return normalized(line["text"]), round(x / page["width"], 2), round(y / page["height"], 2)
    return None


def pdf_metrics(pages):
    sizes = Counter()
    prose_sizes = Counter()
    margins = Counter()
    for page in pages:
        keys = set()
        for line in page["lines"]:
            if not line.get("bbox"):
                continue
            text = line["text"].strip()
            key = margin_key(page, line)
            if key:
                keys.add(key)
            # Weight by text length; a short label or page number is poor
            # evidence of the body face, even if it occurs many times.
            size = round(height(line), 1)
            sizes[size] += len(text)
            if not key and (len(text) > 44 or (len(text) >= 20 and text.endswith((".", "!", "?")))):
                prose_sizes[size] += len(text)
        margins.update(keys)
    typical = prose_sizes or sizes
    body = typical.most_common(1)[0][0] if typical else 0
    return body, {key for key, count in margins.items() if count >= 2}


def pdf_lines(page, columns, margins):
    ordered = []
    for line in columns(page):
        if line.get("bbox"):
            key = margin_key(page, line)
            if key and (key in margins or PAGE_NUMBER.fullmatch(key[0])):
                continue
        ordered.append(line)
    if any(normalized(line["text"]) in TOC for line in ordered[:3]):
        return []
    return ordered


def pdf_context(ordered):
    """Locate shared baselines and line-wrapped titles within source blocks."""
    boxes = [line for line in ordered if line.get("bbox")]
    rows = sorted(boxes, key=lambda line: line["bbox"][1])
    shared, wrapped = set(), set()
    for a, b in zip(rows, rows[1:]):
        if b["bbox"][1] - a["bbox"][1] <= min(height(a), height(b)) * .25:
            shared.update((a["line"], b["line"]))
    previous = {}
    for line in sorted(boxes, key=lambda line: line["line"]):
        last = previous.get(line["block"])
        if (last and line["text"][:1].islower() and abs(height(line) - height(last)) < .04
                and 0 < line["bbox"][1] - last["bbox"][1] < height(last) * 2):
            wrapped.add(last["line"])
        previous[line["block"]] = line
    return shared, wrapped


def pdf_level(line, body_size, parents, blocks, shared, wrapped):
    """A decimal outline gives depth directly; other printed levels use size."""
    if not line.get("bbox") or not body_size or line["line"] in wrapped:
        return None
    text = line["text"].strip()
    if (not text or len(text) > 110 or len(text.split()) > 13 or not (text[0].isupper() or text[0].isdigit())
            or not any(c.isalpha() for c in text) or text.endswith((".", "?", "!", ",", ";"))
            or re.search(r"\.{2,}|[=<>*]|^\s*(?:fig(?:ure)?|table)\s+\d", text, re.I)):
        return None
    face = height(line)
    # A superscript in an ordinary sentence can enlarge the line box.
    # A heading should have large letterforms in the words themselves.
    if any(height(word) < .84 * face for word in line.get("words", [])):
        return None
    if line.get("words"):
        face = min(face, median(height(word) for word in line["words"]))
    chapter = CHAPTER.match(text)
    decimal = NUMBERED.match(text)
    experiment = EXPERIMENT.fullmatch(text)
    label = normalized(text)
    # At body size, short printed labels alone are ambiguous with cells and
    # prose. Require an isolated structural label or hierarchical numbering.
    if face < body_size * 1.07 and not (
        blocks[line["block"]] == 1 and line["line"] not in shared and
        (experiment or (chapter and not text[chapter.end():].lstrip()[:1].islower())
         or (decimal and "." in decimal[1]) or label in PAPER_ROOT)
    ):
        return None
    if chapter:
        return 1
    if decimal:
        return min(9, decimal[1].count(".") + 1)
    if label in PAPER_ROOT:
        return 1
    # Equal-sized paper headings commonly put Methods/Results under an
    # explicit experiment, while a second named experiment starts a peer.
    for level, parent in reversed(parents):
        title = parent["text"].strip()
        if experiment and EXPERIMENT.fullmatch(title):
            return level
        if (face < height(parent) - .035 or CHAPTER.match(title)
                or (EXPERIMENT.fullmatch(title) and label in EXPERIMENT_CHILD
                    and face <= height(parent) + .035)):
            return min(9, level + 1)
    return 1


def iter_sections(document, columns):
    """Yield sections in reading order. Body contains only the direct content."""
    pages = document["pages"]
    pdf = document["format"] == "pdf"
    body_size, margins = pdf_metrics(pages) if pdf else (None, None)
    parents, current = [], None
    for page in pages:
        ordered = pdf_lines(page, columns, margins) if pdf else columns(page)
        if page["status"] != "text" or not ordered:
            if current:
                yield current
            parents, current = [], None
            continue
        if pdf:
            blocks = Counter(line["block"] for line in ordered)
            shared, wrapped = pdf_context(ordered)
        for line in ordered:
            level = pdf_level(line, body_size, parents, blocks, shared, wrapped) if pdf else (
                line.get("level") if line.get("role") == "heading" else None)
            if level is None:
                if current:
                    current["body"].append(line)
                continue
            if current:
                yield current
            while parents and parents[-1][0] >= level:
                parents.pop()
            parents.append((level, line))
            headings = [parent for _, parent in parents]
            current = {"path": [h["text"] for h in headings], "headings": headings,
                       "heading": line, "body": []}
    if current:
        yield current


def matches(label, part):
    return normalized(label) == part or normalized(NUMBERED.sub("", label, count=1)) == part


def find_sections(document, value, columns, citation):
    """Look up a leaf or full path, keeping every independently cited match."""
    parts = [normalized(part) for part in value.split(">")]
    if not all(parts):
        raise ValueError("a section path requires a heading at each level")
    results = []
    active = {}

    def key(line):
        return line["page"], line["line"]

    for section in iter_sections(document, columns):
        headings = section["headings"]
        body = section["body"]
        for ancestor in headings[:-1]:
            result = active.get(key(ancestor))
            if result is not None:
                nested = [section["heading"], *body]
                result["text"].extend(line["text"] for line in nested)
                result["citations"].extend(citation(line) for line in nested)
        active = {key(h): active[key(h)] for h in headings[:-1] if key(h) in active}
        path = section["path"]
        labels = path[-1:] if len(parts) == 1 else path
        if len(labels) != len(parts) or not all(matches(label, part) for label, part in zip(labels, parts)):
            continue
        result = {"path": path, "text": [line["text"] for line in body],
                  "citations": [citation(line) for line in [*headings, *body]]}
        results.append(result)
        active[key(section["heading"])] = result
    for result in results:
        result["text"] = "\n".join(result["text"])
    return results
