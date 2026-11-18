"""Format regressions using generated DOCX and PDF bytes (offline)."""
import hashlib
import io
import os
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch
import zipfile

from . import formats


def public_paper(case):
    directory = os.environ.get("MINDSPACE_PREVIEW_PAPERS")
    if not directory:
        raise unittest.SkipTest("set MINDSPACE_PREVIEW_PAPERS to run the publisher PDF checks")
    source = Path(directory) / case["file"]
    if hashlib.sha256(source.read_bytes()).hexdigest() != case["sha256"]:
        raise ValueError(f"unexpected publisher PDF revision: {source.name}")
    return source


def docx(body):
    word_ns = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
    pkg_ns = "http://schemas.openxmlformats.org/package/2006"
    content_type = "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
    rel_type = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("[Content_Types].xml", f'<Types xmlns="{pkg_ns}/content-types"><Override PartName="/word/document.xml" ContentType="{content_type}"/></Types>')
        archive.writestr("_rels/.rels", f'<Relationships xmlns="{pkg_ns}/relationships"><Relationship Id="rId1" Type="{rel_type}" Target="word/document.xml"/></Relationships>')
        archive.writestr("word/document.xml", f'<w:document xmlns:w="{word_ns}"><w:body>{body}</w:body></w:document>')
    return output.getvalue()


def pdf(text_pages):
    objs = [b"<< /Type /Catalog /Pages 2 0 R >>", None,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
    kids = []
    for content in text_pages:
        number = len(objs) + 1
        kids.append(f"{number} 0 R")
        objs.append(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 240 360] /Resources << /Font << /F1 3 0 R >> >> /Contents {number+1} 0 R >>".encode())
        objs.append(f"<< /Length {len(content)} >>\nstream\n".encode() + content + b"\nendstream")
    objs[1] = f"<< /Type /Pages /Count {len(kids)} /Kids [{' '.join(kids)}] >>".encode()
    data = bytearray(b"%PDF-1.4\n")
    offsets = [0]
    for i, obj in enumerate(objs, start=1):
        offsets.append(len(data))
        data.extend(f"{i} 0 obj\n".encode() + obj + b"\nendobj\n")
    start = len(data)
    data.extend(f"xref\n0 {len(offsets)}\n0000000000 65535 f \n".encode())
    for pos in offsets[1:]:
        data.extend(f"{pos:010d} 00000 n \n".encode())
    data.extend(f"trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n".encode())
    return bytes(data)


class FormatTests(unittest.TestCase):
    def read(self, data, filename="misleading.bin"):
        with TemporaryDirectory() as folder:
            path = Path(folder, filename)
            path.write_bytes(data)
            return formats.extract(path)

    def test_docx_structural_citations_and_no_invented_page(self):
        data = docx("""
          <w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Setup</w:t></w:r></w:p>
          <w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr>
          <w:r><w:t>Turn</w:t><w:tab/><w:t>left</w:t></w:r></w:p>
          <w:tbl><w:tr><w:tc><w:p><w:r><w:t>header</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
          <w:p><w:r><w:drawing/></w:r></w:p>""")
        result = self.read(data, "manual.pdf")
        self.assertEqual(result["format"], "docx")
        self.assertEqual(result["sha256"], hashlib.sha256(data).hexdigest())
        self.assertEqual(result, self.read(data))
        page, = result["pages"]
        self.assertIsNone(page["number"])
        self.assertTrue(page["issues"])
        heading, item, cell = page["lines"]
        self.assertEqual((heading["role"], heading["level"]), ("heading", 2))
        self.assertEqual((item["role"], item["text"], item["num_id"]), ("list_item", "Turn\tleft", "2"))
        self.assertEqual((cell["table"], cell["row"], cell["column"]), (0, 0, 0))

    def test_pdf_real_poppler_boxes_and_blank_page(self):
        result = self.read(pdf([b"BT /F1 12 Tf 20 280 Td (A sample) Tj ET", b""]), "paper.docx")
        self.assertEqual(result["format"], "pdf")
        text, blank = result["pages"]
        self.assertEqual((text["number"], text["width"], text["height"]), (1, 240, 360))
        line, = text["lines"]
        self.assertEqual([w["text"] for w in line["words"]], ["A", "sample"])
        self.assertLess(line["bbox"][0], line["bbox"][2])
        self.assertEqual((blank["number"], blank["status"]), (2, "unreadable"))

    def test_image_with_header_flags_partial_text_page(self):
        # A tiny inline gray image and a selectable page header. Looking only
        # for nonempty text would conceal scanned content on such pages.
        page = b"BT /F1 12 Tf 20 280 Td (Heading) Tj ET\nq 10 0 0 10 30 30 cm\nBI /W 1 /H 1 /CS /G /BPC 8 ID \x80 EI Q"
        result, = self.read(pdf([page]))["pages"]
        self.assertEqual(result["status"], "text")
        self.assertIn("embedded_images_unread", result["issues"])

    def test_pdf_output_bound(self):
        with patch.object(formats, "MAX_OUTPUT", 100), self.assertRaises(formats.DocumentError):
            self.read(pdf([b"BT /F1 12 Tf 20 280 Td (Document) Tj ET"]))

    def test_preserve_inline_breaks_and_exclude_deleted_content(self):
        data = docx("<w:p><w:r><w:t>Before</w:t><w:br/><w:t>After.</w:t></w:r>"
                    "<w:del><w:r><w:t>Old wording</w:t></w:r></w:del></w:p>")
        line, = self.read(data)["pages"][0]["lines"]
        self.assertEqual(line["text"], "Before\nAfter.")

    def test_large_pdf_snapshot(self):
        content = b"BT /F1 12 Tf 20 280 Td (Target) Tj ET"
        data = pdf([content])
        # Padding is inside a valid PDF comment before the final startxref.
        head, tail = data.rsplit(b"startxref", 1)
        data = head + b"%" + b"x" * (32 * 1024**2) + b"\nstartxref" + tail
        result = self.read(data)
        self.assertEqual(result["sha256"], hashlib.sha256(data).hexdigest())
        self.assertEqual(result["pages"][0]["lines"][0]["text"], "Target")

    def test_batch_page_numbers_remain_physical(self):
        content = b"BT /F1 12 Tf 20 280 Td (Target) Tj ET"
        pages = self.read(pdf([content] * 513))["pages"]
        self.assertEqual([p["number"] for p in pages], list(range(1, 514)))
        self.assertEqual(pages[-1]["lines"][0]["text"], "Target")

    def test_format_specific_source_limit_is_inclusive(self):
        for data, limit in [(pdf([b""]), "MAX_PDF_SOURCE"), (docx("<w:p/>"), "MAX_DOCX_SOURCE")]:
            with self.subTest(limit=limit):
                with patch.object(formats, limit, len(data)):
                    self.assertEqual(self.read(data)["sha256"], hashlib.sha256(data).hexdigest())
                with patch.object(formats, limit, len(data) - 1), self.assertRaisesRegex(
                    formats.DocumentError, "source byte limit"
                ):
                    self.read(data)

    def test_total_layout_budget_remains_bounded(self):
        data = pdf([b"BT /F1 12 Tf 20 280 Td (word) Tj ET"] * 4)
        with patch.object(formats, "PAGES_PER_BATCH", 1, create=True), patch.object(
            formats, "MAX_TOTAL_LAYOUT", 1800, create=True
        ), self.assertRaisesRegex(formats.DocumentError, "total layout"):
            self.read(data)

    def test_poppler_control_in_word_keeps_page_with_issue(self):
        original = formats.poppler
        def with_bad_word(args, *rest):
            data = original(args, *rest)
            return data.replace(b">Beacon<", b">Be\x08acon<") if args[0] == "pdftotext" else data
        with patch.object(formats, "poppler", with_bad_word):
            page, = self.read(pdf([b"BT /F1 12 Tf 20 280 Td (Beacon) Tj ET"]))["pages"]
        self.assertEqual(page["lines"][0]["text"], "Be\ufffdacon")
        self.assertIn("undecodable_text", page["issues"])

    def test_refusals(self):
        with self.assertRaises(formats.DocumentError):
            self.read(b"random", "file.pdf")
        with self.assertRaises(formats.DocumentError):
            self.read(b"%PDF-this is broken")
        with patch.object(formats, "MAX_PDF_SOURCE", 8), self.assertRaises(formats.DocumentError):
            self.read(b"%PDF-123456")
        with patch.object(formats, "MAX_PAGES", 1), self.assertRaises(formats.DocumentError):
            self.read(pdf([b"", b""]))
        with patch.object(formats, "MAX_PART", 80), self.assertRaises(formats.DocumentError):
            self.read(docx("<w:p/>"))
        with self.assertRaises(formats.DocumentError):
            formats.parse_xml('<!DOCTYPE a [<!ENTITY foo "bad">]><a>&foo;</a>'.encode("utf-16"))
