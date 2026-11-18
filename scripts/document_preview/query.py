"""Evidence queries over a read-only document extraction. Offsets use extraction order."""

from collections import defaultdict
from itertools import groupby
import re

if __package__:
    from .bestiary_fields import is_resistance_field, resistance_value
    from .bestiary_layout import field_owners
    from .cohorts import cohort_counts
    from .sections import find_sections
else:
    from bestiary_fields import is_resistance_field, resistance_value
    from bestiary_layout import field_owners
    from cohorts import cohort_counts
    from sections import find_sections


def lines(page):
    for index, line in enumerate(page["lines"], 1):
        yield {**line, "page": page["number"], "line": index}


def citation(line):
    keys = ("page", "line", "block", "text", "bbox", "table", "row", "column", "num_id", "level")
    return {k: line[k] for k in keys if k in line}


def search(document, term):
    results = []
    for page in document["pages"]:
        for line in lines(page):
            if term.casefold() in line["text"].casefold():
                results.append({"citations": [citation(line)]})
    return results


def procedure(document, heading):
    located = [line for page in document["pages"] for line in lines(page)]
    sections = []
    for index, title in enumerate(located):
        if title["role"] != "heading" or title["text"].strip().casefold() != heading.casefold():
            continue
        body = []
        for line in located[index + 1:]:
            if line["role"] == "heading" and line["level"] <= title["level"]:
                break
            body.append(line)
        sections.append({
            "heading": title["text"],
            "steps": [line["text"] for line in body if line["role"] == "list_item"],
            "citations": [citation(line) for line in [title, *body]],
        })
    if len(sections) == 1 and not sections[0]["steps"]:
        return []
    return sections


ONES = "zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen seventeen eighteen nineteen".split()
TENS = "twenty thirty forty fifty sixty seventy eighty ninety".split()
NUMBERS = {word: i for i, word in enumerate(ONES)}
for i, tens in enumerate(TENS, 2):
    NUMBERS[tens] = i * 10
    for unit, word in enumerate(ONES[1:10], 1):
        NUMBERS[f"{tens}-{word}"] = i * 10 + unit
COUNT = r"(?:\d{1,3}(?:,\d{3}){1,2}|\d{1,7}|" + "|".join(sorted(NUMBERS, key=len, reverse=True)) + r")"
GROUP = r"(?P<group>(?:(?:new|adult|healthy|eligible) )?(?:participants|subjects|volunteers|patients|students))"
COHORT = rf"(?<![\w.,–—\-])(?P<n>{COUNT})\s+{GROUP}\b"
UNSUPPORTED_COUNT_PREFIX = re.compile(
    r"(?:[<>≤≥±–—-]|\b(?:hundred|thousand|million|between|at least|at most|about|over|under|approximately)"
    r"(?: and)?|\b(?:\d+|" + "|".join(NUMBERS) + r")(?: to| or| and)?)[ \t]*$", re.I)
ATTRIBUTED = re.compile(r"['’]s\s+(?:study|experiment)|\bet al\.|\[\d+(?:[–, -]\d+)*\]", re.I)
FOREIGN = re.compile(r"\b(?:previous(?:ly)?|prior|earlier (?:work|studies)|et al\b|cited|references|pilot|hypothesized|planned|not|per (?:group|arm|condition|cohort)|each (?:group|arm|condition|cohort)|(?:in|from|for|to) (?:the |an? )?(?:(?:control|intervention|experimental|treatment|first|second) )?(?:group|arm|condition|cohort))", re.I)


def sample_size(document, experiment):
    label = rf"\b(?:Experiment|Study)\s+{experiment}\b"
    # A count and experiment must have a direct grammatical relationship.
    before = re.compile(COHORT + rf"\s+(?:completed|participated in|took part in|were (?:recruited|enrolled) (?:for|in))\s+{label}", re.I)
    after = re.compile(label + rf"\s*[:,]?\s*(?:(?:we|the authors)\s+)?(?:included|enrolled|recruited|tested|with|had)\s+{COHORT}", re.I)
    findings = []
    for page in document["pages"]:
        ordered = list(lines(page))
        for _, block in groupby(ordered, key=lambda line: line["block"]):
            evidence = list(block)
            raw = " ".join(line["text"] for line in evidence)
            # Parenthetical ages and other annotations can intervene before the verb.
            for sentence in re.split(r"(?<=[.!?])\s+(?=[A-Z])|;\s*", raw):
                if FOREIGN.search(sentence) or ATTRIBUTED.search(sentence):
                    continue
                plain = re.sub(r"\([^()]{1,90}\)", " ", sentence)
                plain = " ".join(plain.split())
                for pattern in (before, after):
                    for match in pattern.finditer(plain):
                        prefix = plain[:match.start("n")]
                        qualifier = UNSUPPORTED_COUNT_PREFIX.search(prefix)
                        if qualifier and not re.search(r"\b(?:experiment|study)\s+$", prefix[:qualifier.start()], re.I):
                            continue
                        value = match["n"].lower().replace(",", "")
                        findings.append({
                            "experiment": experiment,
                            "sample_size": int(value) if value.isdigit() else NUMBERS[value],
                            "group": match["group"],
                            "citations": [citation(line) for line in evidence],
                        })
        findings.extend(table_counts(ordered, experiment))
    return findings


def table_counts(ordered, experiment):
    """Read three explicit cells: a row key, numeric value and aligned column header."""
    label = re.compile(rf"(?:experiment|study)\s+{experiment}", re.I)
    header = re.compile(r"sample size|participants|total (?:n|participants)|n", re.I)
    for row in ordered:
        if not label.fullmatch(row["text"].strip()):
            continue
        if "table" in row:
            cells = [c for c in ordered if c.get("table") == row["table"]]
            values = [c for c in cells if c["row"] == row["row"] and c["column"] > row["column"]]
        elif row.get("bbox"):
            x, y, right, bottom = row["bbox"]
            cells = [c for c in ordered if c.get("bbox")]
            values = [c for c in cells if c["bbox"][0] >= right and abs(c["bbox"][1] - y) <= (bottom-y)/3]
        else:
            continue
        for value in values:
            if not re.fullmatch(r"\d{1,7}", value["text"].strip()):
                continue
            if "table" in row:
                headings = [c for c in cells if c["column"] == value["column"] and c["row"] == 0 and row["row"] > 0]
            else:
                # Single header row directly above the value, aligned by overlap.
                left, top, right, bottom = value["bbox"]
                headings = [c for c in cells if 0 <= top - c["bbox"][3] <= 2*(bottom-top)
                            and min(right, c["bbox"][2]) > max(left, c["bbox"][0])]
            if len(headings) == 1 and header.fullmatch(headings[0]["text"].strip()):
                yield {"experiment": experiment, "sample_size": int(value["text"]),
                       "group": headings[0]["text"], "citations": [citation(c) for c in (headings[0], row, value)]}


def columns(page):
    """Split a page only across a clear central gutter. Full-width lines delimit bands."""
    located = list(lines(page))
    if not located or not page.get("width") or not all(line.get("bbox") for line in located):
        return located
    middle = page["width"] / 2
    ordered = []
    band = []
    def flush():
        ordered.extend(sorted(band, key=lambda line: (line["bbox"][0] >= middle, line["bbox"][1], line["bbox"][0])))
        band.clear()
    for line in sorted(located, key=lambda line: (line["bbox"][1], line["bbox"][0])):
        x, _, right, _ = line["bbox"]
        if x < middle < right:
            flush()
            ordered.append(line)
        else:
            band.append(line)
    flush()
    return ordered


EDITION = re.compile(r"(?:edition\s+[\w.-]+|(?:first|second|third|\d+(?:st|nd|rd|th)) edition)", re.I)
FIELD = re.compile(r"^([\w -]{1,40}):\s*(.*)")
STAT_LABEL = re.compile(
    r"^(?:HP|AC|Speed|Melee|Ranged|Senses|Skills|Spells|Weaknesses|Immunities|Fort|Ref|Will)\b", re.I)
CONTINUATION = re.compile(r"^(?:only|while|when|until|except|excluding|unless|but|if|and|or|during|against|from|without|\()\b", re.I)

OPEN_VALUE = re.compile(r"[,;:]$|\b(?:resistances?|resists|resistant to|only|while|against|from|until)$", re.I)
HP_ROW = re.compile(r"^(?:HP|Hit Points)\s*:?\s*\d[\d,]*\b.*;", re.I)
NAMED_ACTION = re.compile(r"^(?:[A-Z][\w’'-]*\s+){2}\S")


def adjacent_field_line(previous, line):
    """A wrap stays on its margin and advances a line or crosses a page."""
    a, b = previous.get("bbox"), line.get("bbox")
    if not a or not b:
        return True
    height = max(a[3] - a[1], b[3] - b[1])
    if abs(a[0] - b[0]) > 2 * height:
        return False
    return previous["page"] != line["page"] or (b[1] > a[1] and b[1] - a[3] <= b[3] - b[1])


def continues_value(text, previous):
    return (CONTINUATION.match(text) or OPEN_VALUE.search(previous.rstrip())
            or previous.count("(") > previous.count(")"))


def defense_rows(ordered):
    """Rejoin wrapped PDF fields within their source block before ordering columns."""
    blocks = defaultdict(list)
    for line in ordered:
        blocks[line["block"]].append(line)
    fields, consumed = {}, set()
    for block in blocks.values():
        rows = sorted(block, key=lambda line: line["line"])
        for index, first in enumerate(rows):
            text = first["text"].strip()
            if first["line"] in consumed or not (is_resistance_field(text) or HP_ROW.match(text)):
                continue
            evidence = [first]
            for n in range(index + 1, len(rows)):
                nxt = rows[n]
                value = nxt["text"].strip()
                previous = evidence[-1]
                if (not adjacent_field_line(previous, nxt) or not continues_value(value, text)
                        or STAT_LABEL.match(value) or FIELD.match(value) or NAMED_ACTION.match(value)):
                    break
                evidence.append(nxt)
                text += " " + value
            if is_resistance_field(text):
                fields[first["line"]] = evidence
                consumed.update(line["line"] for line in evidence[1:])
    return fields, consumed


def resistance(document, damage):
    results, edition_lines = [], {}
    owner = field_owner = carry = None
    pending = []

    def finish_field():
        if pending and field_owner:
            wording = resistance_value(" ".join(line["text"] for line in pending), damage)
            if wording:
                name, heading = field_owner
                results.append({"entry": name, "resistance": wording,
                                "citations": [citation(heading), *(citation(line) for line in pending)]})
        pending.clear()

    for page in document["pages"]:
        if page["status"] != "text":
            pending.clear()  # A missing continuation could qualify the value.
            owner = carry = None
            continue
        ordered = columns(page)
        fields, consumed = defense_rows(ordered)
        owners, carry = field_owners(page, ordered, carry, fields)
        other_field_block = None
        for line in ordered:
            if line["line"] in consumed:
                continue
            text = line["text"].strip()
            if line["line"] in fields:
                other_field_block = None
                finish_field()
                field_owner = owner
                if line["line"] in owners:
                    header = owners[line["line"]]
                    field_owner = (header.name, header.source) if header else None
                if field_owner:
                    pending.extend(fields[line["line"]])
                continue
            field = FIELD.match(text)
            if field:
                finish_field()
                other_field_block = line["block"]
                if field[1].casefold() in {"name", "entry", "creature", "monster"}:
                    owner = (field[2].strip(), line) if field[2].strip() else None
                continue
            if EDITION.fullmatch(text):
                finish_field()
                edition_lines[text] = [line]
                owner = None
                continue
            if STAT_LABEL.match(text):
                finish_field()
                continue
            adjacent = pending and adjacent_field_line(pending[-1], line)
            if adjacent and continues_value(text, pending[-1]["text"]):
                pending.append(line)
                continue
            if (line["role"] == "heading" or
                    (len(text.split()) <= 7 and text.istitle() and not re.search(r"[.:;!?]", text))):
                finish_field()
                owner = (text, line)
            else:
                # An adjacent clause may qualify this defense even when the
                # extractor starts a new block or the author starts a new page.
                if adjacent and not NAMED_ACTION.match(text):
                    pending.clear()
                finish_field()
                if line["block"] != other_field_block:
                    owner = None
    finish_field()
    edition = next(iter(edition_lines)) if len(edition_lines) == 1 else None
    for result in results:
        result["edition"] = edition
        if edition:
            result["citations"].append(citation(edition_lines[edition][0]))
    return results, len(edition_lines) > 1


def answer(document, profile, operation, value):
    if not str(value).strip():
        raise ValueError("query value must contain text")
    ambiguous = False
    if operation == "search":
        results = search(document, value)
    elif operation == "section":
        results = find_sections(document, value, columns, citation)
        ambiguous = len(results) > 1
    elif operation == "procedure" and profile == "general":
        results = procedure(document, value.strip())
        ambiguous = len(results) > 1
    elif operation == "sample-size" and profile == "paper" and int(value) > 0:
        results = sample_size(document, int(value))
        ambiguous = len({row["sample_size"] for row in results}) > 1
    elif operation == "cohort-counts" and profile == "paper" and int(value) > 0:
        results, ambiguous = cohort_counts(document, int(value), columns, citation)
    elif operation == "resistance" and profile == "bestiary":
        results, ambiguous = resistance(document, value.strip())
    else:
        raise ValueError("operation requires its matching profile and a valid argument")
    coverage = {
        "total_pages": len(document["pages"]) if document["format"] == "pdf" else None,
        "unreadable_pages": [p["number"] for p in document["pages"] if p["status"] != "text"],
        "issues": [{"page": p["number"], "issues": p["issues"]} for p in document["pages"] if p.get("issues")],
        "scope": "extracted text",
        "exhaustive": False,
    }
    if ambiguous and operation not in {"section", "cohort-counts"}:
        results = []
    return {"sha256": document["sha256"], "format": document["format"], "profile": profile,
            "status": "ambiguous" if ambiguous else "matched" if results else "no_match",
            "results": results, "coverage": coverage}
