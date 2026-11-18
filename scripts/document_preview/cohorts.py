"""Extract explicitly reported experiment-wide enrollment and analysis counts."""
import re
from bisect import bisect_right
from collections import defaultdict
from itertools import groupby

if __package__:
    from .sections import iter_sections
else:
    from sections import iter_sections


EXPERIMENT = r"(?:experiment|study)\s+([0-9]{1,7})(?!\w)"
EXPERIMENT_HEADING = re.compile(rf"^(?:\d+(?:\.\d+)*[.)]?\s+)?{EXPERIMENT}\b", re.I)
EXCLUDED_SECTION = re.compile(
    r"\b(?:references|bibliography|prior work|pilot|planned|subgroups?|"
    r"(?:control|treatment|intervention|experimental) (?:group|arm|condition)|"
    r"(?:arm|group) [A-Z0-9]|sensitivity analys(?:is|es))\b", re.I)
GENERAL_SECTION = re.compile(
    r"(?:materials? and )?methods?|methodology|participants?|subjects?|patients?|"
    r"study population|participant flow|results?|analysis|data analysis|statistical analysis|discussion", re.I)
PEOPLE = r"(?:participants?|individuals?|subjects?|volunteers?|patients?|students?)"
AMOUNT = r"(?P<count>\d{1,3}(?:,\d{3}){1,2}|\d{1,7}|[a-z]+(?:-[a-z]+)?)"
DEMOGRAPHIC = r"(?:\s+\(\d+\s+(?:women|men|female|male)\))?"
MEMBERS = rf"{AMOUNT}\s+{PEOPLE}{DEMOGRAPHIC}"
ANALYSIS = r"(?:(?:the|our)\s+)?(?:(?:final|subsequent|primary|main)\s+)?analys(?:is|es)"
STAGES = ("enrolled", "analyzed")
PASSIVE = re.compile(
    rf"(?:data (?:from|for) )?{MEMBERS}\s+(?:was|were)\s+"
    rf"(?P<stage>enrolled|analy[sz]ed|included in {ANALYSIS})\b", re.I)
ACTIVE = re.compile(
    rf"(?:we |the authors )?(?P<stage>enrolled|analy[sz]ed(?: (?:the )?data (?:from|for))?)\s+{MEMBERS}\b", re.I)
ANALYSIS_SAMPLE = re.compile(rf"{ANALYSIS}\s+(?:included|comprised)\s+{MEMBERS}\b", re.I)
PREFIX = re.compile(rf"^(?:(?:in|for) )?{EXPERIMENT}\b[,:]?\s*", re.I)
SUFFIX = re.compile(rf"\s+(?:in|for) {EXPERIMENT}$", re.I)
INTRO = re.compile(r"^(?:(?:thus|overall|in total|in all|in sum|therefore),?\s+)?(?:a total of\s+)?", re.I)
# An explicit scope or a demographic summary may follow a complete count. Other
# trailing text could restrict the claim to one group, endpoint or condition.
TAIL = re.compile(r"(?:\s+in this (?:[A-Za-z-]+ )?(?:experiment|study))?"
                  r"(?:,\s*however,\s*(?:\w+ (?:was|were) excluded)\b[^.;]*)?"
                  r"(?::\s*mean age\s*=.*)?", re.I)
SENTENCE_END = re.compile(r"(?<!\bal\.)(?<=[.?!])\s+", re.I)
CONJUNCTION = re.compile(r";\s*|,?\s+and\s+(?=(?:(?:we )?(?:enrolled|analy[sz]ed)|\d+\s+(?:participants?|were))\b)"
                         r"|,\s*of whom\s+", re.I)
ELLIPSIS = re.compile(rf"(?:(?:we )?(?P<active>enrolled|analy[sz]ed) {AMOUNT}|"
                      r"(?P<passive_count>\d{1,7}) were (?P<passive>enrolled|analy[sz]ed))", re.I)
OTHER_UNIT = re.compile(r"(?:we )?analy[sz]ed \d{1,7} (?:trials|samples|observations|responses)", re.I)
TABLE_STAGE = re.compile(
    r"(?:(?:total|number of) )?(?:participants? )?(enrolled|analy[sz]ed)(?: participants?)?(?:\s*\(n\))?", re.I)
TABLE_KEY = re.compile(r"experiment|study", re.I)
CAPTION_START = re.compile(r"^table\s+(?:[A-Z]?\d+|[IVX]+)\b[.:\s–—-]*", re.I)
GENERAL_CAPTION = re.compile(
    r"(?:(?:overall|total) )?(?:participant(?:s| flow| counts| enrollment)|cohort (?:flow|counts)|"
    r"(?:enrolled and analy[sz]ed participants|enrollment and analysis))"
    r"(?: (?:by|in each) (?:experiment|study))?[.]?", re.I)
INTEGER = re.compile(r"\d{1,7}|\d{1,3}(?:,\d{3}){1,2}")


def stage_name(text):
    return "enrolled" if text.lower() == "enrolled" else "analyzed"


def scope_index(document, experiment, columns):
    """Associate each body line with its nearest experiment, respecting section boundaries."""
    contexts, excluded = {}, set()
    for section in iter_sections(document, columns):
        headings = section["headings"]
        nearest = None
        blocked = any(EXCLUDED_SECTION.search(line["text"]) for line in headings)
        for line in headings:
            title = line["text"].strip().rstrip(".:")
            label = EXPERIMENT_HEADING.match(title)
            if label:
                nearest = line
                suffix = title[label.end():].strip(" :-–—")
                if suffix and not GENERAL_SECTION.fullmatch(suffix):
                    blocked = True
            elif nearest and not GENERAL_SECTION.fullmatch(title):
                # An unrecognized subsection may refer to a subset or endpoint.
                blocked = True
        if nearest and int(EXPERIMENT_HEADING.match(nearest["text"])[1]) != experiment:
            blocked = True
        for line in [section["heading"], *section["body"]]:
            key = (line["page"], line["line"])
            if blocked:
                excluded.add(key)
            elif nearest:
                contexts[key] = nearest
    return contexts, excluded


def claims(sentence, experiment, scoped, number_words):
    """Only complete, affirmative clauses can establish a cohort stage."""
    sentence = sentence.rstrip().rstrip(".!")
    prefix = PREFIX.match(sentence)
    suffix = SUFFIX.search(sentence)
    labels = re.findall(EXPERIMENT, sentence, re.I)
    if labels:
        if not (prefix or suffix) or any(int(n) != experiment for n in labels):
            return
    elif not scoped:
        return
    start = prefix.end() if prefix else 0
    end = suffix.start() if suffix else len(sentence)
    start += INTRO.match(sentence[start:end]).end()
    supported = []
    for boundary in [*CONJUNCTION.finditer(sentence, start, end), None]:
        stop = boundary.start() if boundary else end
        clause = sentence[start:stop]
        for pattern in (PASSIVE, ACTIVE, ANALYSIS_SAMPLE):
            match = pattern.match(clause)
            if not match or not TAIL.fullmatch(clause[match.end():]):
                continue
            token = match["count"].lower().replace(",", "")
            count = int(token) if token.isdigit() else number_words.get(token)
            if count is None:
                return
            stage = "analyzed" if pattern is ANALYSIS_SAMPLE else stage_name(match["stage"])
            supported.append((stage, count, 0 if prefix else start, suffix.end() if suffix else start + match.end()))
            break
        else:
            short = ELLIPSIS.fullmatch(clause) if supported else None
            if short:
                token = (short["count"] or short["passive_count"]).lower().replace(",", "")
                count = int(token) if token.isdigit() else number_words.get(token)
                if count is None:
                    return
                # The first clause supplies the omitted noun and must also be cited.
                supported.append((stage_name(short["active"] or short["passive"]), count, 0, suffix.end() if suffix else stop))
            elif not (supported and boundary is None and OTHER_UNIT.fullmatch(clause)):
                # Either end of a coordination can qualify the entire statement.
                return
        start = boundary.end() if boundary else end
    yield from supported


def paragraph_claims(lines, owner, experiment, number_words):
    # Element offsets retain just the lines containing this sentence even when
    # a PDF paragraph contains several counts or starts with a run-in heading.
    ends, length, text = [], 0, []
    for line in lines:
        value = line["text"].strip()
        text.append(value)
        length += len(value) + 1
        ends.append(length)
    paragraph = " ".join(text)
    start = 0
    for boundary in [*SENTENCE_END.finditer(paragraph), None]:
        end = boundary.start() if boundary else len(paragraph)
        sentence = paragraph[start:end]
        for stage, count, first, last in claims(sentence, experiment, owner is not None, number_words):
            evidence = lines[bisect_right(ends, start + first):bisect_right(ends, start + last - 1) + 1]
            yield stage, count, ([owner] if owner else []) + evidence
        start = boundary.end() if boundary else end


def table_scope(lines, row, key, headers):
    """A caption applies to the entire matrix, including explicitly numbered rows."""
    if "table" in row:
        table_lines = [i for i, line in enumerate(lines) if line.get("table") == row["table"]]
        nearby = [line for line in lines[max(0, table_lines[0] - 2):table_lines[0]]
                  if line.get("role") == "text"]
        after = lines[table_lines[-1] + 1:table_lines[-1] + 2]
        nearby.extend(line for line in after if line.get("role") == "text")
    else:
        x, top, right, bottom = key["bbox"]
        height = bottom - top
        edge = max((c["bbox"][2] for c in headers), default=right)
        nearby = [c for c in lines if c.get("bbox") and c["bbox"][0] < edge
                  and c["bbox"][2] > x - height
                  and (0 <= top - c["bbox"][3] <= 6 * height
                       or 0 <= c["bbox"][1] - row["bbox"][3] <= 3 * height)]
    for line in nearby:
        caption = line["text"].strip()
        label = CAPTION_START.match(caption)
        if label:
            if "table" in row:
                extra = [c["text"] for c in nearby if line["block"] < c["block"] < key["block"]]
                caption = " ".join([caption, *extra])
            else:
                # A wrapped caption may occupy multiple Poppler blocks.
                rest = sorted((c for c in nearby if c["bbox"][1] >= line["bbox"][3]),
                              key=lambda c: c["bbox"][1])
                end = line["bbox"][3]
                for part in rest:
                    if part["bbox"][1] - end > height or CAPTION_START.match(part["text"].strip()):
                        break
                    caption += " " + part["text"].strip()
                    end = part["bbox"][3]
            if not GENERAL_CAPTION.fullmatch(caption[label.end():].strip()):
                return False
    return True


def table_claims(lines, experiment, excluded):
    """Require an experiment row, a stage header, and a unique integer cell."""
    for row in lines:
        text = row["text"].strip()
        label = re.fullmatch(EXPERIMENT, text, re.I)
        if not label or int(label[1]) != experiment or (row["page"], row["line"]) in excluded:
            continue
        if "table" in row:
            table = [line for line in lines if line.get("table") == row["table"]]
            keys = [c for c in table if c["row"] == 0 and c["column"] == row["column"]]
            headers = [c for c in table if c["row"] == 0 and c["column"] > row["column"]]
            cells = [c for c in table if c["row"] == row["row"] and c["column"] > row["column"]]
        elif row.get("bbox"):
            left, top, right, bottom = row["bbox"]
            height = bottom - top
            above = [c for c in lines if c.get("bbox") and 0 <= top - c["bbox"][3] <= height * 4]
            keys = [c for c in above if left < c["bbox"][2] and c["bbox"][0] < right
                    and TABLE_KEY.fullmatch(c["text"].strip())]
            if len(keys) != 1:
                continue
            cells = [c for c in lines if c.get("bbox") and c["bbox"][0] >= right
                     and abs(c["bbox"][1] - top) <= height / 3]
            # A second table can occupy the same baseline in an adjacent column.
            # Follow only contiguous stage headers after this table's row key.
            adjacent = sorted((c for c in above if c["bbox"][0] >= right
                               and abs(c["bbox"][1] - keys[0]["bbox"][1]) <= height / 3),
                              key=lambda c: c["bbox"][0])
            headers = []
            for cell in adjacent:
                if not TABLE_STAGE.fullmatch(cell["text"].strip()):
                    break
                headers.append(cell)
        else:
            continue
        if (len(keys) != 1 or not TABLE_KEY.fullmatch(keys[0]["text"].strip())
                or not table_scope(lines, row, keys[0], headers)):
            continue
        stages = defaultdict(list)
        for header in headers:
            match = TABLE_STAGE.fullmatch(header["text"].strip())
            if match:
                stages[stage_name(match[1])].append(header)
        for stage, matching in stages.items():
            if len(matching) != 1:
                continue
            header = matching[0]
            if "table" in row:
                aligned = [c for c in cells if c["column"] == header["column"]]
            else:
                x, _, right, _ = header["bbox"]
                aligned = [c for c in cells if c["bbox"][0] < right and c["bbox"][2] > x]
            if len(aligned) == 1 and INTEGER.fullmatch(aligned[0]["text"].strip()):
                yield stage, int(aligned[0]["text"].strip().replace(",", "")), [header, row, aligned[0]]


def cohort_counts(document, experiment, columns, citation):
    # Shared numeric vocabulary from the original sample-size query.
    if __package__:
        from .query import NUMBERS
    else:
        from query import NUMBERS

    contexts, excluded = scope_index(document, experiment, columns)
    findings = {stage: defaultdict(dict) for stage in STAGES}
    for page in document["pages"]:
        if page["status"] != "text":
            continue
        located = columns(page)
        def block(line):
            key = (line["page"], line["line"])
            owner = contexts.get(key)
            owner_key = (owner["page"], owner["line"]) if owner else None
            return (line["block"], owner_key, key in excluded or "table" in line)
        for (_, _, skip), group in groupby(located, key=block):
            if skip:
                continue
            group = list(group)
            owner = contexts.get((group[0]["page"], group[0]["line"]))
            for stage, count, evidence in paragraph_claims(group, owner, experiment, NUMBERS):
                for line in evidence:
                    findings[stage][count][line["page"], line["line"]] = citation(line)
        if "complex_table_structure" not in page.get("issues", ()):
            for stage, count, evidence in table_claims(located, experiment, excluded):
                for line in evidence:
                    findings[stage][count][line["page"], line["line"]] = citation(line)
    if not any(findings.values()):
        return [], False
    result = {"experiment": experiment}
    for stage, counts in findings.items():
        status = "ambiguous" if len(counts) > 1 else "reported" if counts else "not_found"
        result[stage] = {"status": status, "count": next(iter(counts)) if len(counts) == 1 else None,
                         "citations": [site for sites in counts.values() for site in sites.values()]}
    return [result], any(len(counts) > 1 for counts in findings.values())
