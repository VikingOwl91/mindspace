"""Recognize affirmative defenses in English stat-block fields."""

import re


# An inline field on an HP row uses the same labels as a standalone field.
# Accept it only after another defense label, so a quoted stat in an action
# or descriptive paragraph cannot become the creature's own resistance.
LABEL = re.compile(
    r"^(?P<label>(?:damage\s+)?resistances?|armou?r|hp|hit points|"
    r"(?:damage\s+)?(?:immunities|weaknesses|vulnerabilities))\b\s*:?\s*",
    re.I,
)
OTHER_FIELD = re.compile(r"^[\w -]{1,40}:")
ARMOR_PREDICATE = re.compile(r"^(?:resistant\s+to|resists)\s+", re.I)
QUALIFIER = re.compile(
    r"\(|\b(?:only|while|when|if|until|except|unless|during|against|from|"
    r"but|not|excluding|without)\b", re.I,
)
CONFLICT = re.compile(r"\b(?:not (?:resisted|resistant)|immune|immunity|weakness|vulnerable)\b", re.I)
LIST_SEPARATOR = re.compile(r"\s*(?:[,;]\s*(?:and\s+)?|\band\b)\s*", re.I)


def defense_fields(text):
    """Yield only clauses owned by an explicit resistance or armor label."""
    kind = None
    wording = ""
    for index, part in enumerate(text.strip().split(";")):
        part = part.strip()
        label = LABEL.match(part)
        if label:
            if kind:
                yield kind, wording
            name = label["label"].casefold()
            kind = None
            if "resistance" in name:
                kind = "resistance"
            elif name in ("armor", "armour"):
                kind = "armor"
            wording = part[label.end():]
        elif index == 0 or OTHER_FIELD.match(part):
            break
        elif kind:
            wording += "; " + part
    if kind:
        yield kind, wording


def is_resistance_field(text):
    """Whether joining continuations could produce a supported defense."""
    return next(defense_fields(text), None) is not None


def matching_item(wording, damage):
    # A type can appear as an exception or a trigger for a different defense.
    # Only names before the first qualification identify what is resisted.
    if CONFLICT.search(wording):
        return None
    qualifier = QUALIFIER.search(wording)
    names = wording[:qualifier.start()] if qualifier else wording
    items = [s.strip() for s in LIST_SEPARATOR.split(names) if s.strip()]
    term = re.compile(re.escape(damage) + r"(?:\s+damage)?(?:\s+\d+)?", re.I)
    for index, item in enumerate(items):
        if term.fullmatch(item):
            if not qualifier:
                return item
            # Keep shared qualifications with their original list.
            if index != len(items) - 1:
                return wording
            return wording[names.rfind(item):]
    return None


def resistance_value(text, damage):
    """Return the source wording for a matching type, or None to abstain."""
    damage = damage.strip()
    if not damage:
        return None
    for kind, value in defense_fields(text):
        if kind == "resistance":
            result = matching_item(value.strip().rstrip("."), damage)
            if result:
                return result
        else:
            # Armor often describes physical protection first, then an
            # independent resistance. Anchor the positive predicate to a
            # clause boundary to avoid accepting a negation or another actor.
            for clause in value.split(";"):
                clause = clause.strip().rstrip(".")
                predicate = ARMOR_PREDICATE.match(clause)
                if predicate and matching_item(clause[predicate.end():], damage):
                    return clause
    return None
