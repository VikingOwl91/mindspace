"""Cited section lookup in native Word outlines and supported PDF layouts."""
import json
from pathlib import Path
from unittest import TestCase

from . import formats
from .query import citation, columns
from .sections import find_sections, iter_sections
from .test_formats import docx, public_paper


CORPUS = Path(__file__).resolve().parents[2] / "docs/experiments/m8/document-corpus"


def para(text, level=None):
    style = f'<w:pPr><w:pStyle w:val="Heading{level}"/></w:pPr>' if level else ''
    return f'<w:p>{style}<w:r><w:t>{text}</w:t></w:r></w:p>'


def pdf_page(number, *rows):
    return {"status": "text" if rows else "unreadable", "width": 400, "height": 500, "number": number,
            "lines": [{"text": text, "bbox": [20, y, 183, y + height], "block": i, "role": "text"}
                      for i, (text, y, height) in enumerate(rows)]}


def lookup(doc, name):
    return find_sections(doc, name, columns, citation)


class SectionTests(TestCase):
    def manual(self):
        text = docx("".join([para("Field", 1), para("Equipment", 2), para("Take a map."),
                             para("Spare parts", 3), para("One axle."), para("Camp", 1),
                             para("Equipment", 2), para("Bring a tent.")]))
        return {"format": "docx", "pages": formats.docx_pages(text)}

    def test_repeat_and_select_cited_path(self):
        doc = self.manual()
        self.assertEqual([r["path"] for r in lookup(doc, "equipment")],
                         [["Field", "Equipment"], ["Camp", "Equipment"]])
        result, = lookup(doc, " field > equipment ")
        self.assertEqual(result["text"], "Take a map.\nSpare parts\nOne axle.")
        self.assertEqual([c["text"] for c in result["citations"]],
                         ["Field", "Equipment", "Take a map.", "Spare parts", "One axle."])
        self.assertTrue(all(c["page"] is None for c in result["citations"]))
        self.assertFalse(lookup(doc, "Other > Equipment"))

    def test_direct_body_does_not_repeat_child_evidence(self):
        sections = list(iter_sections(self.manual(), columns))
        eq = sections[1]
        self.assertEqual(eq["path"], [line["text"] for line in eq["headings"]])
        self.assertEqual([line["text"] for line in eq["body"]], ["Take a map."])

    def test_pdf_ignores_contents_margins_and_unreadable_continuation(self):
        repeated = ("Field Handbook", 12, 14)
        first = pdf_page(1, repeated, ("Table of Contents", 50, 18), ("1 Equipment ..... 2", 130, 10))
        second = pdf_page(2, repeated, ("Chapter 1", 60, 18), ("1.1 Equipment", 130, 14),
                          ("Take a compass into the field.", 190, 10), ("2", 480, 9))
        fourth = pdf_page(4, repeated, ("Elsewhere the trail continues.", 130, 10))
        doc = {"format": "pdf", "pages": [first, second, pdf_page(3), fourth]}
        row, = lookup(doc, "Chapter 1 > Equipment")
        self.assertEqual(row["text"], "Take a compass into the field.")
        self.assertEqual([c["page"] for c in row["citations"]], [2, 2, 2])
        self.assertFalse(lookup(doc, "Field Handbook"))

    def test_equal_sized_experiment_headers_and_superscript_body(self):
        page = pdf_page(1, ("Experiment 1", 50, 14), ("Materials and methods", 75, 14),
                        ("We enrolled the first group.", 100, 10), ("Experiment 2", 140, 14),
                        ("Materials and methods", 165, 14), ("Spacing with a citation", 190, 14),
                        ("The second group was enrolled.", 215, 10), ("References", 250, 14))
        page["lines"][3]["block"] = page["lines"][4]["block"]
        page["lines"][5]["words"] = [{"bbox": [20, 194, 60, 204]}, {"bbox": [60, 190, 64, 198]}]
        doc = {"format": "pdf", "pages": [page]}
        self.assertEqual([s["path"] for s in lookup(doc, "Materials and methods")],
                         [["Experiment 1", "Materials and methods"], ["Experiment 2", "Materials and methods"]])
        self.assertFalse(lookup(doc, "Spacing with a citation"))
        row, = lookup(doc, "Experiment 2 > Materials and methods")
        self.assertEqual(row["text"], "Spacing with a citation\nThe second group was enrolled.")

    def test_body_size_table_row_cannot_start_an_experiment(self):
        page = pdf_page(1, ("Experiment 2", 100, 10), ("32", 100, 10),
                        ("Other participants were recruited.", 130, 10))
        page["lines"][1]["bbox"] = [185, 100, 198, 110]
        self.assertFalse(lookup({"format": "pdf", "pages": [page]}, "Experiment 2"))

    def test_body_face_references_ends_experiment_scope(self):
        first = pdf_page(1, ("Experiment 1", 40, 15), ("We enrolled some volunteers.", 80, 10),
                         ("References", 125, 10), ("Other authors enrolled 60 participants.", 140, 10))
        sections = list(iter_sections({"format": "pdf", "pages": [first]}, columns))
        self.assertEqual([s["path"] for s in sections], [["Experiment 1"], ["References"]])
        self.assertEqual(sections[0]["body"][0]["text"], "We enrolled some volunteers.")

    def test_published_paper(self):
        case = json.loads((CORPUS / "gold.json").read_text(encoding="utf-8"))["paper"]
        paper = formats.extract(public_paper(case))
        row, = lookup(paper, "Participants")
        self.assertEqual(row["path"], ["Materials and Methods", "Participants"])
        self.assertIn("fifteen new participants", row["text"])
        self.assertNotIn("Stimuli", row["text"])
        self.assertTrue(all(c["page"] == 2 and c["bbox"][0] > 300 for c in row["citations"]))

    def test_original_manual(self):
        manual = formats.extract(CORPUS / "procedure.docx")
        section, = lookup(manual, "Reset the beacon")
        self.assertIn("Reset the beacon", section["path"])
        self.assertTrue(section["text"])
