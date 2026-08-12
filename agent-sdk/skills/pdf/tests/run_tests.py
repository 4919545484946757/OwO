# -*- coding: utf-8 -*-
"""pdf 技能契约测试：生成 / AcroForm 填写 / 渲染校验（>=3 个端到端用例）。"""
import os
import shutil
import subprocess
import sys
import tempfile

from pypdf import PdfReader, PdfWriter
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas


def run_pdftoppm(*args: str) -> None:
    tool = os.environ.get("PDFTOPPM") or "pdftoppm"
    if tool.lower().endswith(".cmd"):
        subprocess.run(
            [os.environ.get("COMSPEC", "cmd.exe"), "/c", tool, *args],
            check=True,
            capture_output=True,
        )
    else:
        subprocess.run([tool, *args], check=True, capture_output=True)


def case1(tmp: str) -> None:
    """文本生成 PDF 并提取断言。"""
    path = os.path.join(tmp, "report.pdf")
    pdf = canvas.Canvas(path, pagesize=A4)
    pdf.drawString(72, 760, "OwO PDF skill gate report 2026")
    pdf.drawString(72, 740, "line two with numbers: 12345")
    pdf.showPage()
    pdf.save()
    reader = PdfReader(path)
    assert len(reader.pages) == 1
    text = reader.pages[0].extract_text() or ""
    assert "OwO PDF skill gate report 2026" in text
    assert "12345" in text
    print("case1 ok: 文本生成 PDF 并提取")


def case2(tmp: str) -> None:
    """AcroForm 字段填写并回读。"""
    src = os.path.join(tmp, "form.pdf")
    pdf = canvas.Canvas(src, pagesize=A4)
    pdf.drawString(72, 760, "Name:")
    pdf.acroForm.textfield(
        name="name",
        x=120,
        y=750,
        width=200,
        height=24,
        borderStyle="inset",
        forceBorder=True,
    )
    pdf.showPage()
    pdf.save()

    reader = PdfReader(src)
    writer = PdfWriter(clone_from=reader)
    writer.update_page_form_field_values(writer.pages[0], {"name": "OwO"})
    out = os.path.join(tmp, "filled.pdf")
    with open(out, "wb") as handle:
        writer.write(handle)

    reread = PdfReader(out)
    fields = reread.get_fields() or {}
    assert "name" in fields
    value = fields["name"].get("/V")
    assert value == "OwO", f"字段值错误：{value!r}"
    print("case2 ok: AcroForm 填写并回读")


def case3(tmp: str) -> None:
    """渲染校验：pdftoppm 输出非空 PNG。"""
    src = os.path.join(tmp, "render.pdf")
    pdf = canvas.Canvas(src, pagesize=A4)
    pdf.drawString(72, 760, "render check")
    pdf.showPage()
    pdf.save()
    prefix = os.path.join(tmp, "page")
    run_pdftoppm("-png", "-r", "72", src, prefix)
    pngs = [name for name in os.listdir(tmp) if name.startswith("page") and name.endswith(".png")]
    assert pngs, "未生成渲染 PNG"
    for name in pngs:
        assert os.path.getsize(os.path.join(tmp, name)) > 0, f"空 PNG：{name}"
    print("case3 ok: 渲染校验非空 PNG")


def main() -> None:
    tmp = tempfile.mkdtemp(prefix="owo-skill-pdf-")
    try:
        case1(tmp)
        case2(tmp)
        case3(tmp)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:  # noqa: BLE001
        print(f"pdf skill gate failed: {error}", file=sys.stderr)
        raise
