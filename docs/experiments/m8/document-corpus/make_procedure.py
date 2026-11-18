"""Rebuild procedure.docx using just the Python standard library."""

from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo
from xml.sax.saxutils import escape

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"


def paragraph(text, props=""):
    return f"<w:p><w:pPr>{props}</w:pPr><w:r><w:t>{escape(text)}</w:t></w:r></w:p>"


def relationship(ident, kind, target):
    return f'<Relationship Id="{ident}" Type="{R}/{kind}" Target="{target}"/>'


def relationships(*items):
    ns = "http://schemas.openxmlformats.org/package/2006/relationships"
    return f'<Relationships xmlns="{ns}">{"".join(items)}</Relationships>'


def make(path):
    heading = '<w:pStyle w:val="Heading1"/>'
    subheading = '<w:pStyle w:val="Heading2"/>'
    number = '<w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>'
    body = [
        paragraph("Beacon station handbook"),
        paragraph("Reset the beacon", heading),
        paragraph("Disconnect the signal lead.", number),
        paragraph("Hold the amber switch for five seconds.", number),
        paragraph("Reconnect the signal lead after the lamp pulses twice.", number),
        paragraph("Reset the clock", heading),
        paragraph("Turn the silver dial to noon."),
        paragraph("Harbor station", heading),
        paragraph("Equipment", subheading),
        paragraph("Take the lead line."),
        paragraph("Hill station", heading),
        paragraph("Equipment", subheading),
        paragraph("Pack the wind gauge."),
    ]
    types = {"document": "document.main", "styles": "styles", "numbering": "numbering"}
    overrides = "".join(
        f'<Override PartName="/word/{name}.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.{kind}+xml"/>'
        for name, kind in types.items()
    )
    parts = {
        "[Content_Types].xml": (
            '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            f'{overrides}</Types>'
        ),
        "_rels/.rels": relationships(relationship("rId1", "officeDocument", "word/document.xml")),
        "word/_rels/document.xml.rels": relationships(
            relationship("rId1", "styles", "styles.xml"),
            relationship("rId2", "numbering", "numbering.xml"),
        ),
        "word/document.xml": f'<w:document xmlns:w="{W}"><w:body>{"".join(body)}<w:sectPr/></w:body></w:document>',
        "word/styles.xml": (
            f'<w:styles xmlns:w="{W}"><w:style w:type="paragraph" w:styleId="Heading1">'
            '<w:name w:val="heading 1"/><w:pPr><w:outlineLvl w:val="0"/></w:pPr>'
            '<w:rPr><w:b/></w:rPr></w:style>'
            '<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/>'
            '<w:pPr><w:outlineLvl w:val="1"/></w:pPr><w:rPr><w:b/></w:rPr></w:style></w:styles>'
        ),
        "word/numbering.xml": (
            f'<w:numbering xmlns:w="{W}"><w:abstractNum w:abstractNumId="0">'
            '<w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/>'
            '<w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="1">'
            '<w:abstractNumId w:val="0"/></w:num></w:numbering>'
        ),
    }
    with ZipFile(path, "w") as archive:
        for name, xml in sorted(parts.items()):
            info = ZipInfo(name, date_time=(2000, 1, 1, 0, 0, 0))
            info.compress_type = ZIP_DEFLATED
            archive.writestr(info, xml.encode("utf-8"))


if __name__ == "__main__":
    make(Path(__file__).with_name("procedure.docx"))
