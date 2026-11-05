"""Smoke-test the release loopback OCR, QR and document HTTP paths."""

import argparse
import base64
import io
import json
from pathlib import Path
import tempfile
import time
import urllib.error
import urllib.request
import uuid
import zipfile

from PIL import Image

import benchmark as bench


def get_json(port, path, value):
    return bench.request(port, path, json.dumps(value).encode(), timeout=20)


def make_pdf(png, password=None):
    image = Image.open(io.BytesIO(png))
    pdf = io.BytesIO()
    image.convert("RGB").save(pdf, format="PDF", resolution=120)
    if password is None:
        return pdf.getvalue()
    from pypdf import PdfWriter
    pdf.seek(0)
    writer = PdfWriter(clone_from=pdf)
    writer.encrypt(password, algorithm="RC4-128")
    encrypted = io.BytesIO()
    writer.write(encrypted)
    return encrypted.getvalue()


def upload_pdf(port, pdf, password=None):
    boundary = uuid.uuid4().hex
    options = {"doc.extractionMode": "fullPage", "pageList": [1]}
    if password is not None:
        options["password"] = password
    body = (
        f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="fixture.pdf"\r\n'
        'Content-Type: application/pdf\r\n\r\n'
    ).encode() + pdf + (
        f'\r\n--{boundary}\r\nContent-Disposition: form-data; name="json"\r\n\r\n'
        f'{json.dumps(options)}\r\n'
        f'--{boundary}--\r\n'
    ).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/api/doc/upload", data=body,
        headers={"Content-Type": f"multipart/form-data; boundary={boundary}"},
    )
    with bench.HTTP.open(request, timeout=20) as response:
        return json.load(response)


def require(condition, value):
    if not condition:
        raise RuntimeError(json.dumps(value, ensure_ascii=False))


def wait_document(port, task_id):
    for _ in range(200):
        status = get_json(port, "/api/doc/result", {"id": task_id})
        require(status.get("code") == 100, status)
        if status["is_done"]:
            break
        time.sleep(0.05)
    require(status.get("state") == "success" and status.get("processed_count") == status.get("pages_count") == 1, status)


def verify(port, extended_qr, artifacts, encrypted_pdf):
    fixture = bench.make_fixtures(None)[0]
    image = get_json(port, "/api/ocr", {"base64": fixture["base64"], "options": {"data.format": "text"}})
    require(image.get("code") == 100 and "离线文字识别" in image.get("data", ""), image)
    cases = [("QRCode", "OCR 二维码测试", {})]
    if extended_qr:
        cases.append(("QRCode", "OCR H 级纠错测试", {"w": 320, "h": 320, "quiet_zone": 3, "ec_level": 2}))
        cases.append(("Code128", "WF-OCR-20261106", {"w": 420, "h": 100, "quiet_zone": 12}))
    formats = []
    for format, message, options in cases:
        generated = get_json(port, "/api/qrcode", {"text": message, "options": {"format": format, **options}})
        require(generated.get("code") == 100, generated)
        require(base64.b64decode(generated["data"]).startswith(b"\xff\xd8"), "Expected JPEG output")
        scanned = get_json(port, "/api/qrcode", {"base64": generated["data"]})
        require(scanned.get("code") == 100 and any(item["text"] == message for item in scanned["data"]), scanned)
        formats.append({"format": scanned["data"][0]["format"], "options": options})

    png = base64.b64decode(fixture["base64"])
    upload = upload_pdf(port, make_pdf(png))
    require(upload.get("code") == 100, upload)
    task_id = upload["data"]
    try:
        wait_document(port, task_id)
        value = get_json(port, "/api/doc/result", {"id": task_id, "is_data": True, "format": "text"})
        require("离线文字识别" in value.get("data", ""), value)
        unread = get_json(port, "/api/doc/result", {"id": task_id, "is_data": True})
        require(unread.get("data") == [], unread)
        download = get_json(port, "/api/doc/download", {"id": task_id, "file_types": ["txtPlain", "pdfLayered", "pdfOneLayer"]})
        require(download.get("code") == 100, download)
        with bench.HTTP.open(download["data"], timeout=20) as response:
            with zipfile.ZipFile(io.BytesIO(response.read())) as archive:
                names = sorted(archive.namelist())
                for name in ("ocr.pdf", "ocr-text.pdf"):
                    require(archive.read(name).startswith(b"%PDF"), names)
                require("离线文字识别" in archive.read("ocr-plain.txt").decode(), names)
                if artifacts is not None:
                    artifacts.mkdir(parents=True, exist_ok=True)
                    for name in ("ocr.pdf", "ocr-text.pdf", "ocr-plain.txt"):
                        (artifacts / name).write_bytes(archive.read(name))
    finally:
        cleared = bench.request(port, f"/api/doc/clear/{task_id}")
        require(cleared.get("code") == 100, cleared)
    try:
        bench.HTTP.open(download["data"], timeout=20).close()
    except urllib.error.HTTPError as error:
        require(error.code == 404, f"Expected 404, got {error.code}")
    else:
        raise RuntimeError("Cleared document download is still accessible")
    output = {"ocr_text": image["data"], "barcode_roundtrip": formats, "document_pages": 1, "exports": names,
              "document_clear": "404 on former download URL"}
    if encrypted_pdf:
        password = "synthetic-test-password"
        encrypted = make_pdf(png, password)
        missing = upload_pdf(port, encrypted)
        if missing.get("code") == 100:
            bench.request(port, f"/api/doc/clear/{missing['data']}")
            raise RuntimeError("Password-protected PDF was accepted without a password")
        upload = upload_pdf(port, encrypted, password)
        require(upload.get("code") == 100, upload)
        task_id = upload["data"]
        try:
            wait_document(port, task_id)
            value = get_json(port, "/api/doc/result", {"id": task_id, "is_data": True, "format": "text"})
            require("离线文字识别" in value.get("data", ""), value)
        finally:
            require(bench.request(port, f"/api/doc/clear/{task_id}").get("code") == 100, "Protected task cleanup failed")
        output["password_protected_pdf"] = "missing password rejected, correct password recognized one page"
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust", type=Path, default=bench.PROJECT / "target" / "release" / "wf-zi-ui-bp.exe")
    parser.add_argument("--models", type=Path, default=bench.PROJECT / "models")
    parser.add_argument("--extended-qr", action="store_true")
    parser.add_argument("--encrypted-pdf", action="store_true", help="Also verify a pypdf-generated protected PDF")
    parser.add_argument("--pdf-artifacts-dir", type=Path, help="Save the two synthetic PDF exports here")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    port = bench.reserve_port()
    command = [str(args.rust.resolve()), "--serve", "--no-hotkey", "--port", str(port), "--models", str(args.models.resolve())]
    with tempfile.TemporaryFile() as diagnostics:
        started = time.perf_counter()
        process = bench.start_hidden(command, bench.PROJECT, diagnostics)
        sampler = bench.ProcessSampler(process.pid)
        try:
            bench.wait_ready(process, port, started)
            result = {"sha256": bench.hash_file(args.rust),
                      **verify(port, args.extended_qr, args.pdf_artifacts_dir, args.encrypted_pdf)}
        finally:
            sampler.close()
            process.wait(timeout=5)
    text = json.dumps(result, ensure_ascii=False, indent=2) + "\n"
    if args.output is not None:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text, encoding="utf-8")
    print(text, end="")


if __name__ == "__main__":
    main()
