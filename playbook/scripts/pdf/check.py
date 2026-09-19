from html.parser import HTMLParser
import hashlib
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
BUILD = ROOT / 'build'


class HeaderLinks(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links = []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == 'a' and 'aria-label' in attrs:
            self.links.append(attrs)


links = HeaderLinks()
links.feed((BUILD / 'index.html').read_text())
labels = [link['aria-label'] for link in links.links]
pdf_index = labels.index('PDF of the specification')
assert pdf_index < labels.index('source on GitHub'), 'PDF icon must precede GitHub'
assert links.links[pdf_index]['href'].endswith('/spec.pdf')
assert all((BUILD / f'0{i}.html').is_file() for i in range(7))
pdf = BUILD / 'spec.pdf'
content = pdf.read_bytes()
assert content.startswith(b'%PDF-') and len(content) > 10000, 'Invalid paper PDF'
info = subprocess.check_output(['pdfinfo', str(pdf)], text=True)
pages = int(re.search(r'^Pages:\s+(\d+)', info, re.M).group(1))
assert pages >= 7, 'Paper is missing chapters'
text = subprocess.check_output(['pdftotext', str(pdf), '-'], text=True)
assert 'Content-addressed deduplication' in ' '.join(text.split())
assert 'Prior art' in text, 'Paper is missing its final chapter'
digest = hashlib.sha256(content).hexdigest()
(BUILD / 'spec.pdf.sha256').write_text(f'{digest}  spec.pdf\n')
print(f'PDF link verified; {pages} pages; {len(content)} bytes; SHA-256 {digest}')
