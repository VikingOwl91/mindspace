from unittest import TestCase, main

from .query import answer


def document(*blocks):
    return {"sha256": "a" * 64, "format": "pdf", "pages": [
        {"number": 1, "width": 200, "status": "text",
         "lines": [dict(text=text, block=i, bbox=None, role="text") for i, text in enumerate(blocks)]}
    ]}


def result(doc, profile, query, value):
    return answer(doc, profile, query, value)


class QueryTests(TestCase):
    def test_cites_own_cohort_and_ignores_other_counts(self):
        doc = document("Prior work reported that Experiment 2 enrolled 80 participants.",
                       "Experiment 2 enrolled 24 volunteers; 12 per arm.", "Study 1 recruited 80 participants.")
        res = result(doc, "paper", "sample-size", 2)
        self.assertEqual([h["sample_size"] for h in res["results"]], [24])
        self.assertEqual(res["results"][0]["citations"][0]["line"], 2)
        self.assertEqual(res["sha256"], doc["sha256"])

    def test_new_cohort_scoped_to_experiment(self):
        doc = document("Twenty healthy volunteers completed Study 1 and twenty-three new students "
                       "(average age 22.4 years) took part in Study 2.")
        res = result(doc, "paper", "sample-size", 2)
        self.assertEqual(res["results"][0]["sample_size"], 23)
        self.assertEqual(res["results"][0]["group"], "new students")

    def test_total_across_studies_and_conflicts(self):
        doc = document("There were 42 participants across Experiment 1 and Experiment 2.")
        self.assertEqual(result(doc, "paper", "sample-size", 2)["status"], "no_match")
        doc = document("Experiment 2 recruited 21 participants.", "Experiment 2 recruited 12 participants.")
        res = result(doc, "paper", "sample-size", 2)
        self.assertEqual((res["status"], res["results"]), ("ambiguous", []))

    def test_unsupported_counts_never_return_a_suffix(self):
        for count in ("one hundred and fifteen", "twenty four", "between 10 and 20", "12–15"):
            doc = document(f"{count} participants completed Study 2.")
            self.assertEqual(result(doc, "paper", "sample-size", 2)["status"], "no_match", count)
        doc = document("1,230 participants completed Experiment 2.")
        self.assertEqual(result(doc, "paper", "sample-size", 2)["results"][0]["sample_size"], 1230)
        doc = document("Wilson’s Experiment 2 recruited 20 participants.")
        self.assertEqual(result(doc, "paper", "sample-size", 2)["status"], "no_match")

    def test_group_counts_are_not_experiment_totals(self):
        for sentence in ("Study 2 included 12 participants per condition.",
                         "Experiment 2 enrolled 15 participants in the control group and 15 participants in the treatment group."):
            self.assertEqual(result(document(sentence), "paper", "sample-size", 2)["status"], "no_match")

    def test_condition_carried_over_page_boundary(self):
        doc = document("Azure Ram", "Damage Resistances: cold")
        continuation = document("only while submerged", "Actions: Bite")["pages"][0]
        continuation["number"] = 2
        doc["pages"].append(continuation)
        row = result(doc, "bestiary", "resistance", "cold")["results"][0]
        self.assertEqual(row["resistance"], "cold only while submerged")
        self.assertEqual([c["page"] for c in row["citations"]], [1, 1, 2])

    def test_unknown_continuation_cannot_make_a_conditional_resistance_unconditional(self):
        for clause in ("provided it is submerged.", "subject to the conditions below."):
            for page_break in (True, False):
                with self.subTest(clause=clause, page_break=page_break):
                    rows = [(15, 10, 100, "Azure Ram"), (15, 30, 178, "Damage Resistances: cold")]
                    pages = [self.layout_page(1, rows)]
                    continuation = (18, 12 if page_break else 44, 185, clause)
                    if page_break:
                        pages.append(self.layout_page(2, [continuation]))
                    else:
                        pages[0] = self.layout_page(1, [*rows, continuation])
                    doc = {"sha256": "x", "format": "pdf", "pages": pages}
                    self.assertEqual(result(doc, "bestiary", "resistance", "cold")["results"], [])

    def test_explicit_name_starts_new_entry(self):
        doc = document("Edition 2", "Ash Wolf", "Damage Resistances: fire", "Name: Ice Fox", "Damage Resistances: cold")
        self.assertEqual(result(doc, "bestiary", "resistance", "cold")["results"][0]["entry"], "Ice Fox")

    def test_table_column_and_row_must_both_match(self):
        doc = document("Experiment", "Volts", "Sample size", "Study 3", "40", "13")
        for i, line in enumerate(doc["pages"][0]["lines"]):
            x, y = (i % 3) * 55, (i // 3) * 16
            line["bbox"] = [x, y, x + 30, y + 10]
        res = result(doc, "paper", "sample-size", 3)
        self.assertEqual(res["results"][0]["sample_size"], 13)
        self.assertEqual([c["text"] for c in res["results"][0]["citations"]], ["Sample size", "Study 3", "13"])
        doc["pages"][0]["lines"][2]["text"] = "Temperature"
        self.assertEqual(result(doc, "paper", "sample-size", 3)["status"], "no_match")

    def test_resistance_requires_affirmative_field_and_owns_conditions(self):
        doc = document("Edition 4", "Brass Wisp", "Damage Resistances: fire, but not cold",
                       "Crystal Bear", "Damage Immunities: cold", "Actions: deals cold damage",
                       "Opal Drake", "Damage Resistances: cold while buried")
        res = result(doc, "bestiary", "resistance", "cold")
        self.assertEqual([(r["edition"], r["entry"], r["resistance"]) for r in res["results"]],
                         [("Edition 4", "Opal Drake", "cold while buried")])
        doc["pages"][0]["lines"].append({"text": "Edition 5", "role": "text", "block": 99, "bbox": None})
        self.assertEqual(result(doc, "bestiary", "resistance", "cold")["status"], "ambiguous")

    def test_unreadable_page_breaks_entry_ownership(self):
        doc = document("Jade Crow")
        doc["pages"].append({"number": 2, "status": "unreadable", "lines": []})
        page3 = document("Resistances: cold")["pages"][0]
        page3["number"] = 3
        doc["pages"].append(page3)
        res = result(doc, "bestiary", "resistance", "cold")
        self.assertEqual(res["coverage"]["unreadable_pages"], [2])
        self.assertEqual(res["status"], "no_match")

    def test_rank_owner_uses_header_span_and_defense_row(self):
        page = self.layout_page(1, [
            (15, 20, 70, "MOSS RAVEN"), (180, 20, 245, "CREATURE 4"),
            (270, 20, 344, "SIDEBAR"), (270, 60, 345, "Lore Notes"),
            (85, 80, 210, "HP 48; Resistances cold 7, fire 2"),
        ])
        doc = {"sha256": "x", "format": "pdf", "pages": [page]}
        hit, = result(doc, "bestiary", "resistance", "cold")["results"]
        self.assertEqual((hit["entry"], hit["resistance"]), ("MOSS RAVEN", "cold 7"))
        self.assertIn("HP 48", " ".join(c["text"] for c in hit["citations"]))

    def test_unresolved_badge_cannot_inherit_an_older_owner(self):
        page = self.layout_page(1, [
            (15, 20, 76, "NIGHT DRAKE"), (180, 20, 245, "CREATURE 3"),
            (180, 70, 245, "CREATURE 6"), (15, 90, 211, "HP 20; Resistances cold 3"),
        ])
        doc = {"sha256": "x", "format": "pdf", "pages": [page]}
        self.assertEqual(result(doc, "bestiary", "resistance", "cold")["results"], [])

    def test_explicit_identity_after_a_ranked_entry(self):
        page = self.layout_page(1, [
            (15, 20, 76, "NIGHT DRAKE"), (180, 20, 245, "CREATURE 3"),
            (15, 80, 93, "Name: Citrus Fox"), (15, 100, 190, "Damage Resistances: cold"),
        ])
        doc = {"sha256": "x", "format": "pdf", "pages": [page]}
        hit, = result(doc, "bestiary", "resistance", "cold")["results"]
        self.assertEqual(hit["entry"], "Citrus Fox")

    def test_numeric_list_wraps_and_ends_at_a_named_ability(self):
        page = self.layout_page(1, [
            (15, 20, 76, "NIGHT DRAKE"), (180, 20, 245, "CREATURE 3"),
            (15, 80, 190, "HP 42; Resistances fire 8,"),
            (20, 92, 190, "cold 3 only while buried"),
            (15, 106, 199, "Quaking Steps Each stride shakes the ground."),
        ])
        for line in page["lines"][2:]:
            line["block"] = 3
        doc = {"sha256": "x", "format": "pdf", "pages": [page]}
        hit, = result(doc, "bestiary", "resistance", "cold")["results"]
        self.assertEqual((hit["entry"], hit["resistance"]), ("NIGHT DRAKE", "cold 3 only while buried"))
        self.assertEqual([c["line"] for c in hit["citations"]], [1, 3, 4])

    def test_hp_row_joins_hanging_lines_across_sidebar_interleaving(self):
        page = self.layout_page(1, [
            (15, 10, 96, "Glass Finch"), (15, 45, 96, "GLASS FINCH\ufffd"), (180, 45, 244, "CREATURE 6"),
            (15, 80, 239, "HP 48; Immunities stunned,"),
            (22, 92, 238, "sleep; Resistances cold 7,"),
            (22, 104, 240, "fire 2"), (15, 116, 110, "Speed 20"),
            (270, 82, 380, "History of the Vale"),
        ])
        for line in page["lines"][3:7]:
            line["block"] = 3
        doc = {"sha256": "x", "format": "pdf", "pages": [page]}
        hit, = result(doc, "bestiary", "resistance", "cold")["results"]
        self.assertEqual((hit["entry"], hit["resistance"]), ("Glass Finch", "cold 7"))
        self.assertEqual([c["line"] for c in hit["citations"]], [1, 4, 5, 6])
        page["lines"][0]["text"] = "River Finch"
        self.assertEqual(result(doc, "bestiary", "resistance", "cold")["results"], [])

    def test_adjacent_column_cannot_complete_a_defense_list(self):
        page = self.layout_page(1, [
            (15, 20, 76, "NIGHT DRAKE"), (160, 20, 195, "CREATURE 3"),
            (15, 80, 195, "HP 42; Immunities stunned,"),
            (220, 92, 395, "sleep; Resistances cold 3"), (15, 104, 95, "Speed 30"),
        ])
        page["lines"][3]["block"] = page["lines"][2]["block"]
        doc = {"sha256": "x", "format": "pdf", "pages": [page]}
        self.assertEqual(result(doc, "bestiary", "resistance", "cold")["results"], [])

    def test_characteristics_table_owns_following_page_armor(self):
        first = self.layout_page(1, [
            (137, 5, 266, "Field Guide"), (15, 245, 103, "Other Creature"),
            (210, 240, 350, "Mire Goose, marsh scavenger"),
            (210, 255, 240, "Char."), (265, 255, 290, "Roll"), (302, 255, 350, "Average"),
            (210, 267, 241, "STR"), (210, 280, 241, "DEX"),
            (210, 300, 380, "Average hit points: 52"),
        ])
        second = self.layout_page(2, [
            (137, 5, 266, "Field Guide"), (213, 80, 360, "Other Sector"),
            (15, 80, 180, "Armor: thick coat; resistant to cold."),
        ])
        doc = {"sha256": "x", "format": "pdf", "pages": [first, second]}
        hit, = result(doc, "bestiary", "resistance", "cold")["results"]
        self.assertEqual(hit["entry"], "Mire Goose")
        self.assertIn("resistant to cold", hit["resistance"])
        self.assertEqual({c["page"] for c in hit["citations"]}, {1, 2})

    @staticmethod
    def layout_page(number, rows):
        return {"number": number, "width": 400, "height": 350, "status": "text",
                "lines": [{"text": text, "block": i, "bbox": [x, y, right, y + 10],
                           "role": "text"} for i, (x, y, right, text) in enumerate(rows)]}

    def test_duplicate_heading_ambiguous(self):
        doc = document("Restart", "Hold the lever.", "Restart")
        doc["format"] = "docx"
        for line in doc["pages"][0]["lines"]:
            line.update(role="heading" if line["text"] == "Restart" else "list_item", level=1)
        self.assertEqual(result(doc, "general", "procedure", "Restart")["status"], "ambiguous")
        self.assertIsNone(result(doc, "general", "search", "lever")["coverage"]["total_pages"])

    def test_explicit_paper_profile_and_literal_search(self):
        doc = document("Section [A]")
        self.assertEqual(len(result(doc, "general", "search", "[a]")["results"]), 1)
        with self.assertRaises(ValueError):
            result(doc, "general", "sample-size", 2)


if __name__ == "__main__":
    main()
