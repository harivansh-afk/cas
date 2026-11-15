"""Transcribe sections/*.md into paper.tex. Run by build.sh."""
import re, sys, pathlib
P = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path(__file__).parent
def inline(s):
    s = s.replace('\\', r'\textbackslash{}')
    s = re.sub(r'([%&_#$])', r'\\\1', s)
    s = re.sub(r'`([^`]+)`', r'\\texttt{\1}', s)
    s = re.sub(r'\[@([^\]]+)\]', lambda m: r'\cite{' + ','.join(k.strip().lstrip('@') for k in m.group(1).split(';')) + '}', s)
    s = re.sub(r'\[TBD[:.]? ?([^\[\]]*(?:\[[^\]]*\][^\[\]]*)*)\]', r'\\TBD{\1}', s)
    s = re.sub(r'\[pilot\]', r'\\pilot{}', s)
    s = re.sub(r'\[check(?:: ([^\]]*))?\]', lambda m: r'\chk{' + (m.group(1) or '') + '}', s)
    s = re.sub(r'\*\*([^*]+)\*\*', r'\\textbf{\1}', s)
    s = re.sub(r'\*([^*]+)\*', r'\\emph{\1}', s)
    s = s.replace('§', r'\S').replace('@@ITEM@@', r'\item')
    return s
def convert(text, abstract=False):
    out, para, lst = [], [], None
    def flush():
        nonlocal para
        if para: out.append(inline(' '.join(para)) + '\n'); para = []
    def endlist():
        nonlocal lst
        if lst: out.append('\\end{%s}\n' % lst); lst = None
    for line in text.splitlines():
        if not line.strip():
            flush();
            continue
        m = re.match(r'(#+) (?:[\d.]+ )?(.*)', line)
        if m:
            flush(); endlist()
            if abstract: continue
            out.append('\\%s{%s}\n' % ({1:'section',2:'subsection',3:'subsubsection'}[len(m.group(1))], inline(m.group(2))))
            continue
        m = re.match(r'(- |\d+\. )(.*)', line)
        if m:
            flush()
            kind = 'itemize' if m.group(1) == '- ' else 'enumerate'
            if lst != kind: endlist(); out.append('\\begin{%s}' % kind); lst = kind
            para = ['@@ITEM@@ ' + m.group(2)]
            continue
        if lst and not line.startswith(' '):
            flush(); endlist()
        para.append(line.strip())
    flush(); endlist()
    return '\n'.join(out)
secs = sorted((P/'sections').glob('*.md'))
body = []
for f in secs:
    t = f.read_text()
    if f.name.startswith('00'):
        body.append('\\begin{abstract}\n' + convert(t, True) + '\\end{abstract}\n')
    else:
        body.append('%% ---- sections/' + f.name + ' ----\n' + convert(t))
head = r'''% Transcribed from sections/*.md. The Markdown is the source of truth:
% edit there, then run build.sh, which regenerates this file.
\documentclass[10pt,twocolumn]{article}
\usepackage[T1]{fontenc}
\usepackage{lmodern}
\usepackage[margin=0.75in]{geometry}
\usepackage{xcolor}
\usepackage{hyperref}
\newcommand{\TBD}[1]{\textcolor{red}{[TBD: #1]}}
\newcommand{\pilot}{\textcolor{orange}{[pilot]}}
\newcommand{\chk}[1]{\textcolor{blue}{[check: #1]}}

\title{Images Are Unique, Blocks Are Not:\\Sharing Disk State Across a Fleet of Agent VMs}
\author{Anonymous}
\date{}

\begin{document}
\maketitle
'''
tail = r'''
\bibliographystyle{plain}
\bibliography{refs}
\end{document}
'''
(P/'paper.tex').write_text(head + '\n'.join(body) + tail)
