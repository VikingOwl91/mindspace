"""Identify the owner of a defense row from nearby printed stat-block headings."""
import re
from dataclasses import dataclass


RANK = re.compile(r"(?:(.+?)\s+)?CREATURE\s+[-−]?\d{1,2}$", re.I)
CHARACTERISTICS = re.compile(r"char\.?|characteristics?", re.I)
ATTRIBUTES = {"STR", "CON", "SIZ", "DEX", "INT", "POW", "CHA", "WIS", "APP", "EDU"}
IDENTITY = re.compile(r"^(?:name|entry|creature|monster):\s*(.*)$", re.I)
TABLE_FIELDS = ATTRIBUTES | {"ROLL", "AVERAGE", "COMBAT", "SKILLS"}


@dataclass
class Heading:
    name: str
    source: dict
    marker: dict
    left: float
    right: float


def name_like(text):
    return (bool(text) and text[0].isupper() and len(text.split()) <= 10
            and re.fullmatch(r"[\w ’'(),.\-–]+", text) is not None)


def same_row(a, b):
    return abs(a[1] - b[1]) <= min(a[3] - a[1], b[3] - b[1]) * 0.35


def rank_name(line, lines):
    text = line["text"].strip()
    if name_like(text):
        return text, line
    # An undecodable trailing glyph can follow an otherwise intact name.
    # Only use the readable spelling if a nearby printed title confirms it.
    if text.endswith("\ufffd") and name_like(text[:-1]):
        x, y, _, bottom = line["bbox"]
        height = bottom - y
        evidence = [other for other in lines if other["text"].strip().casefold() == text[:-1].casefold()
                    and abs(other["bbox"][0] - x) <= height and 0 < y - other["bbox"][1] <= 6 * height]
        if len(evidence) == 1:
            return evidence[0]["text"].strip(), evidence[0]
    return "", line


def headings(page, lines):
    """An unresolved marker still delimits the preceding entry."""
    ranks, tables = [], []
    for line in lines:
        text, box = line["text"].strip(), line["bbox"]
        x, y, right, bottom = box
        height = bottom - y
        rank = RANK.fullmatch(text)
        if rank:
            # Some extractors keep name and rank on one line; others split them.
            if rank[1]:
                names = [(rank[1], line, x)]
            else:
                same = [l for l in lines if same_row(box, l["bbox"]) and l["bbox"][2] <= x]
                boundary = max((l["bbox"][2] for l in same if RANK.fullmatch(l["text"])), default=0)
                names = []
                for candidate in same:
                    box = candidate["bbox"]
                    if box[0] >= boundary and abs(box[3] - box[1] - height) <= height / 4:
                        name, source = rank_name(candidate, lines)
                        if name:
                            names.append((name, source, box[0]))
            for name, source, left in names or [("", line, 0)]:
                ranks.append(Heading(name if name_like(name) else "", source, line, left, right))
        elif CHARACTERISTICS.fullmatch(text):
            aligned = [l for l in lines if abs(l["bbox"][0] - x) <= height]
            labels = {l["text"].strip().upper() for l in aligned if y < l["bbox"][1] <= y + 5 * height}
            if len(labels & ATTRIBUTES) < 2:
                continue
            candidates = [l for l in aligned if 0 < y - l["bbox"][1] <= 2.5 * height]
            if not candidates:
                continue
            source = max(candidates, key=lambda l: l["bbox"][1])
            name = source["text"].strip().split(",", 1)[0].strip()
            if not name_like(name):
                name = ""
            tables.append(Heading(name, source, line, x, page["width"]))
    return ranks, tables


def in_lane(line, left, right, tolerance):
    return left - tolerance <= line["bbox"][0] < right


def interrupted(section, lines, left, right, start, end, tolerance):
    """A new named or prominent heading invalidates the older attribution."""
    title = section.source["bbox"]
    for line in lines:
        box = line["bbox"]
        if not (start < box[1] < end and in_lane(line, left, right, tolerance)):
            continue
        if box[2] > right + tolerance or line["text"].strip().upper() in TABLE_FIELDS:
            continue
        text = line["text"].strip()
        if IDENTITY.match(text):
            return True
        if box[3] - box[1] >= 0.96 * (title[3] - title[1]) and name_like(text):
            # Trait tags and normal body text are smaller than a stat heading.
            if text.isupper() or text.istitle():
                return True
    return False


def field_owners(page, lines, previous, field_lines):
    """Map field lines to a named heading, or None when markers are inconclusive."""
    if not page.get("width") or not lines or any(l.get("bbox") is None for l in lines):
        return {}, None
    ranks, tables = headings(page, lines)
    width = page["width"]
    midpoint = width / 2
    owners = {}
    if ranks or tables or previous:
        for line in lines:
            label = IDENTITY.match(line["text"].strip())
            if label:
                x = line["bbox"][0]
                edge = midpoint if x < midpoint else width
                ranks.append(Heading(label[1].strip(), line, line, x, edge))
    for line in lines:
        if line["line"] not in field_lines:
            continue
        x, y, _, bottom = line["bbox"]
        height = bottom - y
        ranked = [r for r in ranks if r.marker["bbox"][1] < y
                  and r.left - height <= x < r.right]
        if ranked:
            start = max(r.marker["bbox"][1] for r in ranked)
            latest = [r for r in ranked if r.marker["bbox"][1] == start]
            if len(latest) > 1:
                # A sidebar can have a heading on the very same baseline.
                # Only the candidate aligned with this defense margin owns it.
                latest = [r for r in latest if abs(r.left - x) <= height]
            if len(latest) != 1:
                owners[line["line"]] = None
                continue
            header = latest[0]
            blocked = interrupted(header, lines, header.left, header.right, start + height, y, height)
            owners[line["line"]] = header if header.name and not blocked else None
            continue

        # The characteristics-table style normally flows down a column, then
        # to the next column and finally to the first column of the next page.
        lane = midpoint if x >= midpoint else 0
        edge = width if lane else midpoint
        candidates = [h for h in tables if lane <= h.left < edge and h.marker["bbox"][1] < y]
        if candidates:
            header = max(candidates, key=lambda h: h.marker["bbox"][1])
            start = header.marker["bbox"][1]
        elif not lane and previous:
            header = previous
            start = 0
        elif ranks or tables:
            owners[line["line"]] = None
            continue
        else:
            continue
        # Require field and table to share the same column margin (mirrored
        # pages may differ slightly); foreign sidebars don't own this field.
        if not candidates:
            starts = [l["bbox"][0] for l in lines if l["bbox"][0] < midpoint and l["bbox"][2] < midpoint]
            same_margin = bool(starts) and x <= min(starts) + height * 2
        else:
            same_margin = abs(x - header.left) <= height * 2
        blocked = interrupted(header, lines, lane, edge, start, y, height)
        owners[line["line"]] = header if header.name and same_margin and not blocked else None

    # Only a table in the final occupied column can flow onto the next page.
    right_column = any(l["bbox"][0] >= midpoint for l in lines)
    last = [h for h in tables if (h.left >= midpoint) == right_column]
    carry = max(last, key=lambda h: h.marker["bbox"][1], default=None)
    if carry:
        lane = midpoint if right_column else 0
        height = carry.source["bbox"][3] - carry.source["bbox"][1]
        if interrupted(carry, lines, lane, width, carry.marker["bbox"][1], float("inf"), height):
            carry = None
    return owners, carry
