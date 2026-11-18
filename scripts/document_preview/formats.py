"""Offline extraction for the trusted-file M8 preview (Linux + Poppler)."""
import hashlib
import io
import math
import os
from pathlib import Path
import re
import resource
import signal
import stat
import struct
import subprocess
import time
from xml.etree import ElementTree as ET
import zipfile
import zlib


MAX_DOCX_SOURCE = 32 * 1024 * 1024
MAX_PDF_SOURCE = 384 * 1024 * 1024
MAX_PART = 8 * 1024 * 1024
MAX_OUTPUT = 16 * 1024 * 1024
MAX_PAGES = 1024
PAGES_PER_BATCH = 16
MAX_TOTAL_LAYOUT = 96 * 1024 * 1024
PDF_TIMEOUT = 120
XML_CONTROLS = re.compile(rb"[\x00-\x08\x0b-\x0c\x0e-\x1f]")
MAX_NODES = 180_000

W = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"
X = "{http://www.w3.org/1999/xhtml}"
PKG = "http://schemas.openxmlformats.org/package/2006"
MAIN_TYPE = "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
OFFICE_REL = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
XHTML_DOCTYPE = b'<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd">'


class DocumentError(ValueError):
    """The preview could not safely interpret this document."""


class BoundedTree(ET.TreeBuilder):
    def __init__(self):
        super().__init__()
        self.nodes = 0
        self.depth = 0

    def start(self, tag, attrs):
        self.nodes += 1
        self.depth += 1
        if self.nodes > MAX_NODES or self.depth > 128:
            raise DocumentError("XML structure exceeds preview limits")
        return super().start(tag, attrs)

    def end(self, tag):
        self.depth -= 1
        return super().end(tag)

    def doctype(self, name, pubid, system):
        raise DocumentError("XML document type declarations are unsupported")


def parse_xml(data):
    try:
        return ET.fromstring(data, parser=ET.XMLParser(target=BoundedTree()))
    except (ET.ParseError, LookupError, ValueError) as exc:
        if isinstance(exc, DocumentError):
            raise
        raise DocumentError("invalid document XML") from exc


def xml_part(archive, name):
    try:
        member = archive.getinfo(name)
    except KeyError as exc:
        raise DocumentError(f"missing package part {name}") from exc
    if member.file_size > MAX_PART:
        raise DocumentError("DOCX part exceeds the byte limit")
    if member.flag_bits & 1 or member.compress_type not in (zipfile.ZIP_DEFLATED, zipfile.ZIP_STORED):
        raise DocumentError("encrypted or unsupported DOCX compression")
    with archive.open(member) as source:
        data = source.read(MAX_PART + 1)
    if len(data) != member.file_size:
        raise DocumentError("invalid DOCX part size")
    return parse_xml(data)


def prop(node, path, default=None):
    child = node.find("/".join(W + tag for tag in path.split("/")))
    return default if child is None else child.get(W + "val", default)


def paragraph_text(node, issues):
    if node.tag == W + "t":
        return node.text or ""
    if node.tag in (W + "br", W + "cr"):
        return "\n"
    if node.tag == W + "tab":
        return "\t"
    if node.tag in (W + "drawing", W + "pict", W + "object"):
        issues.add("embedded_objects_unread")
        return ""
    if node.tag in (W + "del", W + "moveFrom", W + "instrText", W + "pPr", W + "rPr"):
        return ""
    if node.tag not in (W + "p", W + "r", W + "hyperlink", W + "ins", W + "smartTag", W + "moveTo", W + "fldSimple"):
        if len(node):
            issues.add("unsupported_structure")
        return ""
    return "".join(paragraph_text(child, issues) for child in node)


def docx_pages(data):
    # ZipFile parses its entire central directory on open. Limit that allocation
    # before it happens and exclude multipart/ZIP64 archives from this preview.
    end = data.rfind(b"PK\x05\x06", max(0, len(data) - 65557))
    if end < 0 or len(data) - end < 22:
        raise DocumentError("invalid ZIP directory")
    disk, directory_disk, local, count, size, offset, comment = struct.unpack_from("<4H2IH", data, end + 4)
    if (disk or directory_disk or local != count or count > 4096 or size > 1024 * 1024
            or end + 22 + comment != len(data) or offset + size != end):
        raise DocumentError("unsupported or oversized ZIP directory")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        names = archive.namelist()
        if len(names) != count or len(names) != len(set(names)):
            raise DocumentError("ambiguous or inconsistent ZIP directory")
        content_types = xml_part(archive, "[Content_Types].xml")
        parts = [elem.get("ContentType") for elem in content_types.findall(f"{{{PKG}/content-types}}Override")
                 if elem.get("PartName") == "/word/document.xml"]
        rels = xml_part(archive, "_rels/.rels")
        office = [elem for elem in rels.findall(f"{{{PKG}/relationships}}Relationship")
                  if elem.get("Type") == OFFICE_REL]
        if (parts != [MAIN_TYPE] or len(office) != 1 or office[0].get("TargetMode", "Internal") != "Internal"
                or office[0].get("Target") not in ("word/document.xml", "/word/document.xml")):
            raise DocumentError("unsupported DOCX main document part")
        root = xml_part(archive, "word/document.xml")
        if root.tag != W + "document" or root.find(W + "body") is None:
            raise DocumentError("invalid Word document body")
        lines = []
        issues = set()
        if any(re.match(r"word/(footer|header|footnotes|endnotes)[^/]*\.xml$", n) for n in names):
            issues.add("ancillary_parts_unread")
        table = 0
        for block, element in enumerate(root.find(W + "body")):
            base = {"block": block, "bbox": None, "words": []}
            if element.tag == W + "p":
                text = paragraph_text(element, issues).strip()
                if not text:
                    continue
                style = prop(element, "pPr/pStyle", "")
                heading = re.fullmatch(r"heading\s*([1-9])", style, re.I)
                outline = prop(element, "pPr/outlineLvl", "")
                level = prop(element, "pPr/numPr/ilvl", "0")
                num = prop(element, "pPr/numPr/numId")
                if heading or outline in tuple("012345678"):
                    base.update(role="heading", level=int(heading[1]) if heading else int(outline) + 1)
                elif (num and num != "0") or style.lower().startswith(("listnumber", "listbullet")):
                    if not re.fullmatch("[0-8]", level):
                        raise DocumentError("invalid DOCX list level")
                    base.update(role="list_item", num_id=num, level=int(level))
                else:
                    base.update(role="text")
                lines.append(dict(base, text=text))
            elif element.tag == W + "tbl":
                for row, tr in enumerate(element.findall(W + "tr")):
                    for column, tc in enumerate(tr.findall(W + "tc")):
                        if tc.find(W + "tbl") is not None or tc.find(W + "tcPr/" + W + "gridSpan") is not None:
                            issues.add("complex_table_structure")
                        text = "\n".join(paragraph_text(p, issues).strip() for p in tc.findall(W + "p"))
                        if text.strip():
                            lines.append(dict(base, role="table_cell", table=table, row=row, column=column, text=text))
                table += 1
            elif element.tag != W + "sectPr":
                issues.add("unsupported_structure")
        return [{"number": None, "status": "text" if lines else "unreadable",
                 "issues": sorted(issues), "lines": lines}]


def poppler_limits():
    # Convert writes past RLIMIT_FSIZE into EFBIG, avoiding a fatal signal.
    signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
    for key, bound in ((resource.RLIMIT_CORE, 0), (resource.RLIMIT_AS, 384 * 1024**2),
                       (resource.RLIMIT_FSIZE, MAX_OUTPUT + 1), (resource.RLIMIT_CPU, 12)):
        soft, _hard = resource.getrlimit(key)
        if soft != resource.RLIM_INFINITY:
            bound = min(bound, soft)
        resource.setrlimit(key, (bound, bound))


def poppler(args, snapshot, deadline):
    # An anonymous file enforces the stdout cap in the child (a pipe cannot).
    # Each child opens the same seekable, private snapshot. A page batch need
    # not read the rest of a large PDF through a pipe.
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise DocumentError("PDF preview exceeded its time limit")
    fd = snapshot.fileno()
    command = [*args, f"/proc/self/fd/{fd}"]
    if args[0] == "pdftotext":
        command.append("-")
    with os.fdopen(os.memfd_create("document-preview"), "w+b") as output:
        try:
            run = subprocess.run(command, pass_fds=(fd,),
                                 stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.DEVNULL,
                                 preexec_fn=poppler_limits, timeout=min(15, remaining), check=False,
                                 env={"PATH": os.defpath, "LC_ALL": "C"})
        except (OSError, subprocess.SubprocessError) as exc:
            raise DocumentError(f"{args[0]} unavailable or exceeded its time/resource limit") from exc
        if os.fstat(output.fileno()).st_size > MAX_OUTPUT:
            raise DocumentError("PDF layout exceeds output limit")
        if run.returncode:
            raise DocumentError(f"{args[0]} could not read this PDF within preview limits")
        output.seek(0)
        return output.read()


def coords(element, keys):
    try:
        values = [float(element.attrib[key]) for key in keys]
    except (ValueError, KeyError) as exc:
        raise DocumentError("invalid PDF geometry") from exc
    if not all(math.isfinite(v) for v in values):
        raise DocumentError("non-finite PDF geometry")
    return values


def layout_page(page, number, image_pages):
    width, height = coords(page, ("width", "height"))
    if width <= 0 or height <= 0:
        raise DocumentError("invalid PDF page size")
    lines = []
    block = 0
    issues = {"embedded_images_unread"} if number in image_pages else set()
    for flow, group in enumerate(page.findall(X + "flow")):
        for box in group.findall(X + "block"):
            for line in box.findall(X + "line"):
                bbox_keys = ("xMin", "yMin", "xMax", "yMax")
                words = [{"text": w.text, "bbox": coords(w, bbox_keys)}
                         for w in line.findall(X + "word") if w.text and w.text.strip()]
                if words:
                    text = " ".join(word["text"] for word in words)
                    if "\ufffd" in text:
                        issues.add("undecodable_text")
                    lines.append({"text": text, "block": block, "flow": flow, "bbox": coords(line, bbox_keys),
                                  "words": words, "role": "text"})
            block += 1
    return {"number": number, "width": width, "height": height,
            "status": "text" if lines else "unreadable", "issues": sorted(issues), "lines": lines}


def pdf_pages(snapshot):
    deadline = time.monotonic() + PDF_TIMEOUT
    info = poppler(["pdfinfo"], snapshot, deadline)
    counts = re.findall(rb"^Pages:[ \t]+([0-9]{1,9})[ \t]*$", info, re.M)
    if len(counts) != 1:
        raise DocumentError("PDF page count is missing or ambiguous")
    count = int(counts[0])
    if not 1 <= count <= MAX_PAGES:
        raise DocumentError(f"PDF page count exceeds the supported range (1 to {MAX_PAGES})")
    # A page with a running header and a scanned body otherwise looks readable.
    images = poppler(["pdfimages", "-list"], snapshot, deadline)
    image_pages = {int(m[1]) for m in re.finditer(rb"^\s*(\d+)\s+\d+\s", images, re.M)}
    result = []
    total = 0
    for start in range(1, count + 1, PAGES_PER_BATCH):
        end = min(start + PAGES_PER_BATCH - 1, count)
        args = ["pdftotext", "-f", str(start), "-l", str(end), "-enc", "UTF-8", "-bbox-layout"]
        markup = poppler(args, snapshot, deadline)
        total += len(markup)
        if total > MAX_TOTAL_LAYOUT:
            raise DocumentError("PDF exceeds total layout byte limit")
        # Some fonts map glyphs to XML-forbidden controls. Preserve the gap as a
        # replacement character, which layout_page reports as undecodable text.
        markup = XML_CONTROLS.sub(b"\xef\xbf\xbd", markup.removeprefix(XHTML_DOCTYPE))
        pages = parse_xml(markup).findall(f"{X}body/{X}doc/{X}page")
        if len(pages) != end - start + 1:
            raise DocumentError("incomplete PDF page range")
        result.extend(layout_page(page, n, image_pages) for n, page in enumerate(pages, start))
    return result


def extract(path: Path) -> dict:
    """Read a bounded snapshot and expose evidence with its exact source revision."""
    try:
        with os.fdopen(os.open(path, os.O_RDONLY | os.O_NONBLOCK), "rb") as source:
            meta = os.fstat(source.fileno())
            if not stat.S_ISREG(meta.st_mode):
                raise DocumentError("expected a regular document file")
            header = source.peek(1024)
            if header.startswith(b"PK\x03\x04"):
                if meta.st_size > MAX_DOCX_SOURCE:
                    raise DocumentError("DOCX exceeds the source byte limit")
                data = source.read(MAX_DOCX_SOURCE + 1)
                if len(data) > MAX_DOCX_SOURCE:
                    raise DocumentError("DOCX exceeds the source byte limit")
                return {"sha256": hashlib.sha256(data).hexdigest(), "format": "docx",
                        "pages": docx_pages(data)}
            if not header.lstrip(b" \r\n\t").startswith(b"%PDF-"):
                raise DocumentError("expected a PDF or DOCX file")
            if meta.st_size > MAX_PDF_SOURCE:
                raise DocumentError(f"PDF exceeds the source byte limit ({MAX_PDF_SOURCE // 1024**2} MiB)")
            digest = hashlib.sha256()
            with os.fdopen(os.memfd_create("document-source"), "w+b") as snapshot:
                while chunk := source.read(1024 * 1024):
                    if snapshot.tell() + len(chunk) > MAX_PDF_SOURCE:
                        raise DocumentError(f"PDF exceeds the source byte limit ({MAX_PDF_SOURCE // 1024**2} MiB)")
                    digest.update(chunk)
                    snapshot.write(chunk)
                snapshot.flush()
                return {"sha256": digest.hexdigest(), "format": "pdf", "pages": pdf_pages(snapshot)}
    except (OSError, zipfile.BadZipFile, NotImplementedError, zlib.error) as exc:
        raise DocumentError("unreadable document or damaged archive") from exc
