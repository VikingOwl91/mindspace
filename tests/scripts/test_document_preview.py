"""Exercise the checkout-only CLI against the independently authored document corpus."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))
from scripts.document_preview.test_formats import docx, pdf, public_paper


CORPUS = ROOT / "docs/experiments/m8/document-corpus"
GOLD = json.loads((CORPUS / "gold.json").read_text(encoding="utf-8"))


class PreviewAcceptance(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.work = Path(temp.name)
        # Exercise the standalone preview with only its modules and the supplied
        # document available. The CLI has no reason to consult a checkout.
        self.cli = self.work / "preview/cli.py"
        shutil.copytree(ROOT / "scripts/document_preview", self.cli.parent,
                        ignore=shutil.ignore_patterns("test_*.py", "__pycache__"))

    def document(self, case):
        path = self.work / "input.unknown"
        source = public_paper(case) if "publisher_pdf" in case else CORPUS / case["file"]
        shutil.copyfile(source, path)
        return path

    def query(self, path, fmt, profile, *args):
        run = subprocess.run(
            [sys.executable, "-B", str(self.cli), str(path), "--profile", profile, *args],
            cwd=self.work, capture_output=True, text=True, timeout=50,
        )
        self.assertEqual(run.returncode, 0, run.stderr)
        response = json.loads(run.stdout)
        self.assertEqual(response["sha256"], hashlib.sha256(path.read_bytes()).hexdigest())
        self.assertEqual(response["format"], fmt)
        self.assertEqual(response["profile"], profile)
        return response

    def test_procedure_is_a_numbered_section(self):
        case = GOLD["procedure"]
        response = self.query(self.document(case), "docx", "general", "procedure", case["heading"])
        self.assertEqual(response["status"], "matched")
        result, = response["results"]
        self.assertEqual(result["heading"], case["heading"])
        self.assertEqual(result["steps"], case["steps"])
        citations = result["citations"]
        self.assertEqual([c["text"] for c in citations], [case["heading"], *case["steps"]])
        self.assertTrue(all(c["page"] is None for c in citations))
        blocks = [c["block"] for c in citations]
        self.assertEqual(blocks, sorted(set(blocks)))
        for c in citations[1:]:
            self.assertIsNotNone(c["num_id"])
        for excluded in case["excluded"]:
            self.assertNotIn(excluded, json.dumps(result))

    def test_revised_docx_returns_the_revised_instruction(self):
        case = GOLD["procedure"]
        original = self.document(case)
        revised = original.with_name("revision.bin")
        old = case["steps"][1]
        new = "Press the green button once."
        with zipfile.ZipFile(original) as src, zipfile.ZipFile(revised, "w") as dst:
            for entry in src.infolist():
                data = src.read(entry)
                if entry.filename == case["location"]["part"]:
                    self.assertIn(old.encode(), data)
                    data = data.replace(old.encode(), new.encode())
                dst.writestr(entry, data)
        result, = self.query(revised, "docx", "general", "procedure", case["heading"])["results"]
        self.assertEqual(result["steps"][1], new)
        self.assertIn(new, [c["text"] for c in result["citations"]])
        self.assertNotIn(old, json.dumps(result))

    def test_reported_experiment_uses_its_own_participants(self):
        case = GOLD["paper"]
        path = self.document(case)
        response = self.query(path, "pdf", "paper", "sample-size", str(case["experiment"]))
        self.assertEqual(response["status"], "matched")
        result, = response["results"]
        self.assertEqual(result["sample_size"], case["participants"])
        self.assertEqual(result["experiment"], case["experiment"])
        self.assertEqual(result["group"], case["group"])
        evidence = result["citations"]
        self.assertEqual({c["page"] for c in evidence}, {case["page"]})
        # This 612pt-wide publisher PDF has the evidence in the right column.
        self.assertTrue(all(c["bbox"][0] > 306 for c in evidence))
        words = " ".join(c["text"] for c in evidence)
        self.assertIn(case["evidence"], words)
        missing = self.query(path, "pdf", "paper", "sample-size", "99")
        self.assertEqual((missing["status"], missing["results"]), ("no_match", []))

    def test_repeated_manual_headings_and_a_qualified_path(self):
        case = GOLD["equipment"]
        path = self.document(case)
        ambiguous = self.query(path, "docx", "general", "section", case["heading"])
        self.assertEqual(ambiguous["status"], "ambiguous")
        self.assertCountEqual([row["path"] for row in ambiguous["results"]],
                              [match["path"] for match in case["matches"]])
        for match in case["matches"]:
            response = self.query(path, "docx", "general", "section", " > ".join(match["path"]))
            self.assertEqual(response["status"], "matched")
            row, = response["results"]
            self.assertEqual((row["path"], row["text"]), (match["path"], match["text"]))
            self.assertEqual([c["text"] for c in row["citations"]], [*match["path"], match["text"]])
            self.assertTrue(all(c["page"] is None for c in row["citations"]))

    def test_participation_section_has_physical_citations_without_cohort_claims(self):
        case = GOLD["paper"]
        path = self.document(case)
        section = self.query(path, "pdf", "paper", "section", " > ".join(case["section"]))
        self.assertEqual(section["status"], "matched")
        row, = section["results"]
        self.assertEqual(row["path"], case["section"])
        self.assertIn(case["evidence"], " ".join(row["text"].split()))
        self.assertNotIn("Stimuli", row["text"])
        self.assertTrue(all(c["page"] == case["page"] and c["bbox"][0] > 306 for c in row["citations"]))
        counts = self.query(path, "pdf", "paper", "cohort-counts", str(case["experiment"]))
        self.assertEqual((counts["status"], counts["results"]), ("no_match", []))

    def test_publisher_experiment_has_separate_enrollment_and_analysis_evidence(self):
        case = GOLD["stroop"]
        path = self.document(case)
        response = self.query(path, "pdf", "paper", "cohort-counts", str(case["experiment"]))
        self.assertEqual(response["sha256"], case["sha256"])
        self.assertEqual(response["status"], "matched")
        row, = response["results"]
        self.assertEqual(row["experiment"], case["experiment"])
        for stage in ("enrolled", "analyzed"):
            measure = row[stage]
            self.assertEqual((measure["status"], measure["count"]), ("reported", case[stage]["count"]))
            self.assertEqual({c["page"] for c in measure["citations"]}, {case["page"]})
            self.assertTrue(all(c["bbox"] for c in measure["citations"]))
            words = " ".join(c["text"] for c in measure["citations"])
            self.assertIn(case[stage]["evidence"], words)
        recruited = self.query(path, "pdf", "paper", "cohort-counts", "1")
        self.assertEqual((recruited["status"], recruited["results"]), ("no_match", []))

    def test_publisher_repeated_methods_headings_keep_their_experiment_and_page(self):
        case = GOLD["stroop"]
        path = self.document(case)
        matches = case["methods"]
        choices = self.query(path, "pdf", "paper", "section", "Materials and methods")
        self.assertEqual(choices["status"], "ambiguous")
        self.assertCountEqual([row["path"] for row in choices["results"]], [m["path"] for m in matches])
        for match in matches:
            response = self.query(path, "pdf", "paper", "section", " > ".join(match["path"]))
            self.assertEqual(response["status"], "matched")
            row, = response["results"]
            self.assertEqual(row["path"], match["path"])
            for heading in match["path"]:
                self.assertTrue(any(c["text"] == heading and c["page"] == match["page"] and c["bbox"]
                                    for c in row["citations"]))

    def test_one_reported_stage_and_a_conflict_in_the_other(self):
        path = self.work / "cohort.unknown"
        body = ('<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr>'
                '<w:r><w:t>Experiment 2</w:t></w:r></w:p>'
                '<w:p><w:r><w:t>We enrolled 31 participants.</w:t></w:r></w:p>')
        path.write_bytes(docx(body))
        partial = self.query(path, "docx", "paper", "cohort-counts", "2")
        self.assertEqual(partial["status"], "matched")
        row, = partial["results"]
        self.assertEqual((row["enrolled"]["status"], row["enrolled"]["count"]), ("reported", 31))
        self.assertEqual(row["analyzed"], {"status": "not_found", "count": None, "citations": []})
        table = []
        for values in (("Experiment", "Analyzed"), ("Experiment 1", "18"), ("Experiment 2", "25")):
            cells = "".join(f"<w:tc><w:p><w:r><w:t>{value}</w:t></w:r></w:p></w:tc>" for value in values)
            table.append(f"<w:tr>{cells}</w:tr>")
        path.write_bytes(docx(body + "<w:tbl>" + "".join(table) + "</w:tbl>"))
        combined = self.query(path, "docx", "paper", "cohort-counts", "2")
        self.assertEqual(combined["status"], "matched")
        row, = combined["results"]
        self.assertEqual((row["enrolled"]["count"], row["analyzed"]["count"]), (31, 25))
        evidence = row["analyzed"]["citations"]
        self.assertEqual([(c["row"], c["column"], c["text"]) for c in evidence],
                         [(0, 1, "Analyzed"), (2, 0, "Experiment 2"), (2, 1, "25")])
        for count in (25, 27):
            body += f'<w:p><w:r><w:t>We analyzed {count} participants.</w:t></w:r></w:p>'
        path.write_bytes(docx(body))
        conflict = self.query(path, "docx", "paper", "cohort-counts", "2")
        self.assertEqual(conflict["status"], "ambiguous")
        row, = conflict["results"]
        self.assertEqual(row["enrolled"]["count"], 31)
        self.assertEqual((row["analyzed"]["status"], row["analyzed"]["count"]), ("ambiguous", None))
        evidence = [c["text"] for c in row["analyzed"]["citations"]]
        self.assertIn("We analyzed 25 participants.", evidence)
        self.assertIn("We analyzed 27 participants.", evidence)

    def field_guide(self, *pages):
        # Coordinates are in PDF points. Each string is a separate text object,
        # so Poppler determines which words and headings form a block.
        streams = []
        for page in pages:
            objects = [f"BT /F1 {size} Tf {x} {y} Td ({text}) Tj ET"
                       for x, y, size, text in page]
            streams.append("\n".join(objects).encode("ascii"))
        path = self.work / "field-guide.unknown"
        path.write_bytes(pdf(streams))
        return self.query(path, "pdf", "bestiary", "resistance", "cold")

    def test_ranked_defenses_distinguish_adjacent_entries_and_sidebar(self):
        field = "HP 53; Weaknesses salt 4; Resistances cold 8, fire 2"
        response = self.field_guide([
            (7, 337, 8, "Edition 9"),
            (7, 288, 8, "Harbor Lore"), (7, 275, 6, "A cold gale rises."),
            (83, 288, 9, "MARSH LARK"), (178, 288, 9, "CREATURE 4"),
            (83, 273, 7, "Perception +8"), (83, 255, 6, field),
            (83, 217, 9, "EMBER DOG"), (178, 217, 9, "CREATURE 3"),
            (83, 202, 6, "HP 22; Immunities cold; Resistances fire 11"),
            (83, 192, 6, "Melee frost snap deals cold damage."),
        ])
        self.assertEqual(response["status"], "matched")
        row, = response["results"]
        self.assertEqual((row["entry"], row["resistance"], row["edition"]),
                         ("MARSH LARK", "cold 8", "Edition 9"))
        for expected in ("MARSH LARK", field):
            self.assertTrue(any(c["page"] == 1 and c["bbox"][0] >= 82
                                and expected in c["text"] for c in row["citations"]))
        self.assertIs(response["coverage"]["exhaustive"], False)

    def test_stat_table_owner_carries_to_armor_on_following_page(self):
        header = (95, 340, 8, "Field Notes")
        armor = "Armor: resistant to cold while dormant."
        response = self.field_guide([
            header,
            (6, 306, 8, "Tidal currents"), (6, 284, 6, "Small waves move east."),
            (132, 196, 10, "DELTA MOUSE"),
            (132, 183, 7, "Char."), (160, 183, 7, "Roll"), (192, 183, 7, "Average"),
            (132, 167, 7, "STR"), (160, 167, 7, "3D6"), (192, 167, 7, "45"),
            (132, 153, 7, "CON"), (160, 153, 7, "3D6"), (192, 153, 7, "50"),
            (132, 135, 7, "Move: 7"),
        ], [
            header, (6, 305, 6, armor), (6, 288, 6, "Skills: Forage 55%."),
            (135, 305, 8, "Name: Bramble Wight"), (135, 288, 6, "Armor: immune to cold."),
        ])
        self.assertEqual(response["status"], "matched")
        row, = response["results"]
        self.assertEqual(row["entry"], "DELTA MOUSE")
        self.assertIn("resistant to cold while dormant", row["resistance"])
        self.assertIsNone(row["edition"])
        self.assertTrue(any(c["page"] == 1 and c["text"] == "DELTA MOUSE"
                            and c["bbox"][0] > 120 for c in row["citations"]))
        self.assertTrue(any(c["page"] == 2 and c["text"] == armor
                            and c["bbox"][0] < 10 for c in row["citations"]))
        self.assertIs(response["coverage"]["exhaustive"], False)

    def test_bestiary_tracks_split_entries_and_reports_unread_pages(self):
        case = GOLD["bestiary"]
        response = self.query(self.document(case), "pdf", "bestiary", "resistance", case["value"])
        self.assertEqual(response["status"], "matched")
        rows = response["results"]
        self.assertCountEqual([r["entry"] for r in rows], [m["entry"] for m in case["matches"]])
        by_name = {r["entry"]: r for r in rows}
        for match in case["matches"]:
            row = by_name[match["entry"]]
            self.assertEqual(row["edition"], case["edition"])
            self.assertEqual(row["resistance"], match["wording"])
            citations = row["citations"]
            self.assertTrue(any(c["page"] == match["entry_page"] and c["text"] == match["entry"]
                                for c in citations))
            self.assertTrue(any(c["text"] == case["edition"] for c in citations))
            field = " ".join(c["text"] for c in citations if c["page"] == match["evidence_page"])
            self.assertIn(case["field"] + ": " + match["wording"], field)
        coverage = response["coverage"]
        self.assertEqual(coverage["unreadable_pages"], case["unreadable_pages"])
        self.assertEqual(coverage["total_pages"], case["total_pages"])
        self.assertIs(coverage["exhaustive"], False)


if __name__ == "__main__":
    unittest.main()
