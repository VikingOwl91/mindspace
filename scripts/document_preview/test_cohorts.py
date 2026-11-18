from unittest import TestCase

from .cohorts import cohort_counts
from .query import citation, columns


def document(*paragraphs):
    lines = []
    for index, item in enumerate(paragraphs):
        level, text = item if isinstance(item, tuple) else (None, item)
        lines.append({"text": text, "block": index, "role": "heading" if level else "text",
                      "level": level, "bbox": None})
    return {"format": "docx", "pages": [{"number": None, "status": "text", "lines": lines}]}


def query(doc):
    return cohort_counts(doc, 2, columns, citation)


class CohortTests(TestCase):
    def test_separate_stages_and_ancestor_evidence(self):
        doc = document((1, "Experiment 1"), "We enrolled 51 volunteers.",
                       (1, "Experiment 2"), (2, "Methods"), (3, "Participants"),
                       "Seventeen volunteers were enrolled in this experiment, however, three were excluded.",
                       "Thus, 14 participants (8 women) were included in the subsequent analyses: mean age = 37.2 years.",
                       (1, "Study 3"), "7 participants were analyzed.")
        (hit,), ambiguous = query(doc)
        self.assertFalse(ambiguous)
        for stage, expected, sites in [("enrolled", 17, [3, 6]), ("analyzed", 14, [3, 7])]:
            self.assertEqual((hit[stage]["count"], hit[stage]["status"]), (expected, "reported"))
            self.assertEqual([c["line"] for c in hit[stage]["citations"]], sites)

    def test_explicit_statement_can_supply_just_one_stage(self):
        doc = document("For Study 2, we enrolled 1,200 subjects.", "Subjects completed the task.")
        (hit,), ambiguous = query(doc)
        self.assertFalse(ambiguous)
        self.assertEqual(hit["enrolled"]["count"], 1200)
        self.assertEqual(hit["analyzed"], {"status": "not_found", "count": None, "citations": []})
        (hit,), _ = query(document("Data from eleven participants were analysed in Experiment 2."))
        self.assertEqual(hit["analyzed"]["count"], 11)
        self.assertEqual(hit["enrolled"]["status"], "not_found")

    def test_partial_conflict_does_not_erase_independent_stage(self):
        doc = document((1, "Study 2"), "We enrolled 31 participants.", "We enrolled 32 participants.",
                       "Our final analysis included 28 participants.")
        (hit,), ambiguous = query(doc)
        self.assertTrue(ambiguous)
        self.assertEqual((hit["enrolled"]["status"], hit["enrolled"]["count"]), ("ambiguous", None))
        self.assertEqual(hit["analyzed"]["count"], 28)
        self.assertEqual({c["line"] for c in hit["enrolled"]["citations"]}, {1, 2, 3})

    def test_foreign_nonexact_or_nonstage_counts_abstain(self):
        for sentence in ("20 individuals were recruited.", "21 subjects completed the experiment.",
                         "We planned to enroll 45 participants.", "We did not enroll 39 participants.",
                         "Approximately 18 participants were enrolled.", "Between 14 and 19 people enrolled.",
                         "We enrolled 12 participants in each group.", "Only 9 participants in the control group were analyzed.",
                         "We enrolled 18 participants for Experiment 1.", "Jones et al. enrolled 17 participants.",
                         "We analyzed data from 27 participants in the pilot study.",
                         "A total of 20 trials were analyzed.", "16 percent of participants were analyzed.",
                         "We analyzed data from 19 of the 20 participants."):
            with self.subTest(sentence=sentence):
                self.assertEqual(query(document((1, "Experiment 2"), sentence)), ([], False))
        self.assertEqual(query(document((1, "Study 2"), (2, "References"),
                                        "Study 2 enrolled 21 participants.")), ([], False))
        self.assertEqual(query(document((1, "Study 2"), (2, "Treatment arm"),
                                        "We analyzed 12 participants.")), ([], False))

    def test_two_stage_clauses_and_sentence_scoped_citations(self):
        doc = document((1, "Experiment 2"), "Participant flow. We enrolled 16 patients and we analyzed 14 patients.",
                       "All individuals completed the survey.")
        (hit,), _ = query(doc)
        self.assertEqual((hit["enrolled"]["count"], hit["analyzed"]["count"]), (16, 14))
        for statement in ("In Study 2, we enrolled 16 patients and analyzed 14.",
                          "Study 2 enrolled 16 volunteers, of whom 14 were analyzed."):
            (hit,), _ = query(document(statement))
            self.assertEqual((hit["enrolled"]["count"], hit["analyzed"]["count"]), (16, 14))
        (hit,), _ = query(document("In Study 2, we enrolled 16 patients and analyzed 14 trials."))
        self.assertEqual(hit["analyzed"]["status"], "not_found")

    def test_coordination_carries_uncertainty_and_qualifications(self):
        for prefix in ("In a previous study, we enrolled 12 participants",
                       "In the control group, we enrolled 12 participants",
                       "Our hypothesis was that we enrolled 12 participants",
                       "We excluded 12 participants",
                       "We planned to enroll 12 participants"):
            for join in (" and ", "; "):
                line = prefix + join + "we analyzed 11 participants."
                with self.subTest(line=line):
                    self.assertEqual(query(document((1, "Experiment 2"), line)), ([], False))
        for line in ("We enrolled 12 participants and we analyzed 11 participants in the placebo group.",
                     "We enrolled 12 participants and analyzed 11 participants?",
                     "We enrolled 12 participants in the pilot and we analyzed 11 participants.",
                     "Study 2 enrolled 12 volunteers and we analyzed 11 participants if they passed screening."):
            with self.subTest(line=line):
                self.assertEqual(query(document((1, "Experiment 2"), line)), ([], False))
        (hit,), _ = query(document((1, "Experiment 2"), "We enrolled 12 participants; we analyzed 11 participants."))
        self.assertEqual((hit["enrolled"]["count"], hit["analyzed"]["count"]), (12, 11))

    def test_implicit_scope_requires_experiment_wide_subsections(self):
        for heading in ("Placebo arm", "Female participants", "Secondary endpoint", "Experiment 2a", "Bespoke subgroup"):
            with self.subTest(heading=heading):
                doc = document((1, "Experiment 2"), (2, "Methods"), (3, heading),
                               "We analyzed 19 participants.", (2, "Results"), "We enrolled 34 participants.")
                (hit,), _ = query(doc)
                self.assertEqual(hit["enrolled"]["count"], 34)
                self.assertEqual(hit["analyzed"]["status"], "not_found")

    def test_captions_can_restrict_both_table_totals(self):
        cells = ["Experiment", "Enrolled", "Analyzed", "Experiment 2", "24", "16"]
        for fmt in ("docx", "pdf"):
            for caption, expect in (("Table 1. Participants in the placebo arm", False),
                                    ("Table 1. Female participants", False),
                                    ("Table 1. Participant flow by experiment", True)):
                with self.subTest(fmt=fmt, caption=caption):
                    doc = document(caption, *cells)
                    page = doc["pages"][0]
                    doc["format"] = fmt
                    if fmt == "pdf":
                        page.update(number=2, width=400, height=500)
                        page["lines"][0].update(bbox=[10, 20, 235, 30])
                    for i, line in enumerate(page["lines"][1:]):
                        if fmt == "docx":
                            line.update(table=0, role="table_cell", row=i // 3, column=i % 3)
                        else:
                            x, y = 10 + i % 3 * 80, 35 + i // 3 * 15
                            line.update(bbox=[x, y, x + 55, y + 10])
                    if expect:
                        (hit,), _ = query(doc)
                        self.assertEqual(hit["enrolled"]["count"], 24)
                        # A qualifier can wrap below a seemingly general caption.
                        continuation = dict(page["lines"][0], text="in the intervention arm")
                        if fmt == "pdf":
                            page["lines"][0]["bbox"] = [45, 10, 179, 20]
                            continuation["bbox"] = [48, 21, 195, 31]
                        page["lines"].insert(1, continuation)
                        for i, line in enumerate(page["lines"]):
                            line["block"] = i
                        self.assertEqual(query(doc), ([], False))
                    else:
                        self.assertEqual(query(doc), ([], False))

    def test_table_requires_named_row_column_and_experiment_total(self):
        cells = ["Experiment", "Enrolled", "Participants analyzed", "Control group",
                 "Study 2", "120", "99", "25", "Study 3", "88", "45", "34"]
        doc = document(*cells)
        for i, line in enumerate(doc["pages"][0]["lines"]):
            line.update(table=0, role="table_cell", row=i // 4, column=i % 4)
        (hit,), ambiguous = query(doc)
        self.assertFalse(ambiguous)
        self.assertEqual([c["text"] for c in hit["analyzed"]["citations"]],
                         ["Participants analyzed", "Study 2", "99"])
        self.assertEqual(hit["enrolled"]["count"], 120)
        doc["pages"][0]["issues"] = ["complex_table_structure"]
        self.assertEqual(query(doc), ([], False))

    def test_pdf_table_geometry_keeps_counts_on_their_row(self):
        cells = ["Study", "Enrolled", "Analyzed", "Study 1", "61", "57", "Experiment 2", "41", "32"]
        doc = document(*cells)
        doc["format"] = "pdf"
        page = doc["pages"][0]
        page.update(number=1, height=500, width=350)
        for i, line in enumerate(page["lines"]):
            x, y = 10 + (i % 3) * 80, 50 + (i // 3) * 14
            line.update(bbox=[x, y, x + 60, y + 10])
        (hit,), _ = query(doc)
        self.assertEqual((hit["enrolled"]["count"], hit["analyzed"]["count"]), (41, 32))
        page["lines"][0]["text"] = "Author"
        self.assertEqual(query(doc), ([], False))

    def test_adjacent_pdf_tables_cannot_supply_each_others_stage(self):
        texts = ["Study", "Enrolled", "Study", "Analyzed", "Study 2", "45", "Study 4", "72"]
        doc = document(*texts)
        doc["format"] = "pdf"
        page = doc["pages"][0]
        page.update(number=1, height=500, width=350)
        for i, line in enumerate(page["lines"]):
            x, y = 10 + (i % 4) * 80, 50 + (i // 4) * 14
            line.update(bbox=[x, y, x + 50, y + 10])
        (hit,), _ = query(doc)
        self.assertEqual(hit["enrolled"]["count"], 45)
        self.assertEqual(hit["analyzed"]["status"], "not_found")
