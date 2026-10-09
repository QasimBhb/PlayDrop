# Cross-checks the SwiftUI files, which cannot be compiled on Linux, against the symbols of
# the modules that do compile there: the app's and the extension's non-SwiftUI files,
# PlayDropCore and PlayDropShared (symbol graphs written by typecheck-linux.sh), plus the
# declarations made in the SwiftUI files themselves. Reports member accesses and calls
# whose name or argument labels do not exist, and uses of another file's private members.
#
# Heuristic, not a compiler: it follows `Type.member`, `Type(...)` and `var.member`, where
# the variable's type is known from `@Environment(T.self) var x`, `@State var x = T(...)`,
# `x: T` annotations and parameters, plus one hop through a known property type. Implicit
# member expressions (`.foo`), SwiftUI's own API and result types beyond one hop are not
# checked. Labels of functions declared inside SwiftUI files are not parsed.
#
# Usage: python3 -I ui-symbols.py <symbol-graph-dir> <apps/ios> <swift files...>
# Exits 1 when it reports anything.
import json, os, re, sys

SG, IOS = sys.argv[1], sys.argv[2]
files = sys.argv[3:]

types = {}  # dotted type path -> {member: [{labels, static, access, rtype, params}]}
def add(path, member, labels, static, access, rtype, params=None):
    types.setdefault(path, {}).setdefault(member, []).append(
        dict(labels=labels, static=static, access=access, rtype=rtype, params=params))

def title_split(title):
    m = re.match(r'^([A-Za-z_][A-Za-z0-9_]*)\((.*)\)$', title)
    if not m:
        return title, None
    labels = [l for l in m.group(2).split(':') if l != '']
    return m.group(1), labels

def params_of(sym):
    """[(label, has_default, is_closure)] from the declaration, or None when not a call."""
    text = ''.join(f['spelling'] for f in sym.get('declarationFragments', []))
    i = text.find('(')
    if i < 0:
        return None
    depth, j = 0, i
    for j in range(i, len(text)):
        if text[j] in '([<':
            depth += 1
        elif text[j] in ')]>' and not (text[j] == '>' and text[j - 1] == '-'):
            depth -= 1
            if depth == 0:
                break
    inner = text[i + 1:j]
    parts, depth, cur = [], 0, ''
    for c in inner:
        if c in '([<':
            depth += 1
        elif c in ')]>':
            depth -= 1
        if c == ',' and depth == 0:
            parts.append(cur); cur = ''
        else:
            cur += c
    if cur.strip():
        parts.append(cur)
    out = []
    for p in parts:
        head = p.split(':', 1)[0].strip().split()
        label = head[0] if head else '_'
        out.append((label, '=' in p.split(':', 1)[-1], '->' in p))
    return out

def frag_type(sym):
    frags = sym.get('declarationFragments', [])
    seen_colon = False
    for f in frags:
        if f['kind'] == 'text' and ':' in f['spelling']:
            seen_colon = True
        elif f['kind'] == 'text' and '->' in f['spelling']:
            seen_colon = True
        elif seen_colon and f['kind'] == 'typeIdentifier':
            text = ''.join(x['spelling'] for x in frags)
            arr = re.search(r':\s*\[' + re.escape(f['spelling']) + r'\]', text)
            return ('[' + f['spelling'] + ']') if arr else f['spelling']
    return None

for name in ('PlayDrop', 'PlayDropShare', 'PlayDropCore', 'PlayDropShared'):
    g = json.load(open(os.path.join(SG, f'{name}.symbols.json')))
    for s in g['symbols']:
        pc = s['pathComponents']
        kind = s['kind']['identifier']
        acc = s.get('accessLevel', 'internal')
        if kind in ('swift.struct', 'swift.class', 'swift.enum', 'swift.protocol', 'swift.typealias'):
            types.setdefault('.'.join(pc), {})
            continue
        owner = '.'.join(pc[:-1])
        if not owner:
            continue
        mname, labels = title_split(pc[-1])
        static = kind.startswith('swift.type.') or kind == 'swift.enum.case'
        if kind == 'swift.init':
            mname = 'init'
        add(owner, mname, labels, static, acc, frag_type(s),
            params_of(s) if labels is not None else None)

# ---- Declarations in the SwiftUI files themselves -------------------------------------
def strip(src):
    out, i, n = [], 0, len(src)
    while i < n:
        if src.startswith('//', i):
            j = src.find('\n', i); i = n if j < 0 else j
        elif src.startswith('/*', i):
            j = src.find('*/', i); i = n if j < 0 else j + 2
        elif src.startswith('"""', i):
            j = src.find('"""', i + 3); out.append('""'); i = n if j < 0 else j + 3
        elif src[i] == '"':
            j = i + 1; depth = 0
            while j < n:
                if src[j] == '\\' and j + 1 < n and src[j + 1] == '(':
                    depth += 1; j += 2; continue
                if depth and src[j] == ')':
                    depth -= 1
                elif src[j] == '\\':
                    j += 2; continue
                elif src[j] == '"' and depth == 0:
                    break
                j += 1
            out.append('""'); i = j + 1
        else:
            out.append(src[i]); i += 1
    return ''.join(out)

decl_re = re.compile(r'\b(struct|class|enum|extension|protocol|actor)\s+([A-Z][A-Za-z0-9_.]*)')
member_re = re.compile(r'\b(static\s+|class\s+)?(?:private\s+|fileprivate\s+|public\s+|internal\s+|@MainActor\s+|nonisolated\s+|mutating\s+|override\s+|private\(set\)\s+|internal\(set\)\s+)*(func|var|let|case)\s+([A-Za-z_][A-Za-z0-9_]*)')
sources = {f: strip(open(f).read()) for f in files}
for f, src in sources.items():
    stack = []  # (type path, brace depth of its body)
    depth = 0
    pending = None
    for m in re.finditer(r'[{}]|' + decl_re.pattern + '|' + member_re.pattern, src):
        tok = m.group(0)
        if tok == '{':
            depth += 1
            if pending:
                stack.append((pending, depth)); pending = None
            continue
        if tok == '}':
            if stack and stack[-1][1] == depth:
                stack.pop()
            depth -= 1
            continue
        if m.group(1):
            name = m.group(2)
            if m.group(1) == 'extension' or not stack:
                pending = name
            else:
                pending = stack[-1][0] + '.' + name
            types.setdefault(pending, {})
            continue
        if m.group(4) and stack and stack[-1][1] == depth:
            # Labels of functions declared in SwiftUI files are not parsed: any call matches.
            add(stack[-1][0], m.group(5), None, bool(m.group(3)), 'internal', None)

def balanced(s, start):
    depth = 0
    for j in range(start, len(s)):
        c = s[j]
        if c in '([{':
            depth += 1
        elif c in ')]}':
            depth -= 1
            if depth == 0:
                return j
    return -1

def call_labels(args):
    parts, depth, cur = [], 0, ''
    for c in args:
        if c in '([{':
            depth += 1
        elif c in ')]}':
            depth -= 1
        if c == ',' and depth == 0:
            parts.append(cur); cur = ''
        else:
            cur += c
    if cur.strip():
        parts.append(cur)
    labels = []
    for p in parts:
        m = re.match(r'\s*([A-Za-z_][A-Za-z0-9_]*)\s*:(?!:)', p)
        labels.append(m.group(1) if m else '_')
    return labels

def matches(called, params):
    """Called labels fit the parameters in order; skipped ones need a default or must be
    closures (trailing closure syntax drops their labels)."""
    k = 0
    for label, has_default, is_closure in params:
        if k < len(called) and called[k] == label:
            k += 1
        elif not (has_default or is_closure):
            return False
    return k == len(called)

def subseq(called, declared):
    it = iter(declared)
    return all(any(c == d for d in it) for c in called)

def resolve(owner):
    if owner in types:
        return owner
    for k in types:
        if k.endswith('.' + owner):
            return k
    return None

# Optional's map/flatMap show up when a variable is an optional of a known type.
ignored_members = {'id', 'rawValue', 'hashValue', 'description', 'self', 'init', 'shared', 'map', 'flatMap'}
problems = []
def check(f, line, owner, member, called):
    path = resolve(owner)
    if not path:
        return
    entries = types[path].get(member)
    if not entries:
        if member in ignored_members or member in ('allCases',):
            return
        problems.append(f'{f}:{line}: {path} has no member `{member}`')
        return
    if called is None:
        return
    ok = False
    for e in entries:
        if e['labels'] is None:
            ok = True; break
        if e.get('params') is not None:
            if matches(called, e['params']):
                ok = True; break
        elif subseq(called, e['labels']):
            ok = True; break
    if not ok:
        decl = ' | '.join('(' + ':'.join(e['labels'] or []) + (':' if e['labels'] else '') + ')' for e in entries)
        problems.append(f'{f}:{line}: {path}.{member}({":".join(called)}{":" if called else ""}) does not match {decl}')

for f, src in sources.items():
    rel = os.path.relpath(f, IOS)
    lineof = lambda pos: src.count('\n', 0, pos) + 1
    vars_ = {}
    for m in re.finditer(r'@Environment\(\s*([A-Z][A-Za-z0-9_]*)\.self\s*\)\s*(?:private\s+)?var\s+([a-z][A-Za-z0-9_]*)', src):
        vars_[m.group(2)] = m.group(1)
    for m in re.finditer(r'(?:@State|@Bindable)\s+(?:private\s+)?var\s+([a-z][A-Za-z0-9_]*)\s*=\s*([A-Z][A-Za-z0-9_.]*)(?:\(|\.shared)', src):
        vars_[m.group(1)] = m.group(2)
    for m in re.finditer(r'\b(?:let|var)\s+([a-z][A-Za-z0-9_]*)\s*:\s*([A-Z][A-Za-z0-9_]*)\b(?!\s*[<\[])', src):
        vars_.setdefault(m.group(1), m.group(2))
    for m in re.finditer(r'[(,]\s*(?:_\s+)?([a-z][A-Za-z0-9_]*)\s*:\s*([A-Z][A-Za-z0-9_]*)\s*[,)=]', src):
        vars_.setdefault(m.group(1), m.group(2))
    # Type.member and Type.member(...)
    for m in re.finditer(r'(?<![.\w])([A-Z][A-Za-z0-9_]*(?:\.[A-Z][A-Za-z0-9_]*)*)\.([a-z_][A-Za-z0-9_]*)', src):
        owner, member = m.group(1), m.group(2)
        if resolve(owner) is None:
            continue
        after = m.end()
        called = None
        if after < len(src) and src[after] == '(':
            end = balanced(src, after)
            called = call_labels(src[after + 1:end])
        check(rel, lineof(m.start()), owner, member, called)
    # Type(...) initialisers of app types
    for m in re.finditer(r'(?<![.\w])([A-Z][A-Za-z0-9_]*)\(', src):
        owner = m.group(1)
        path = resolve(owner)
        if not path or path not in types:
            continue
        inits = types[path].get('init')
        if not inits:
            continue  # memberwise or view initialisers declared implicitly
        end = balanced(src, m.end() - 1)
        called = call_labels(src[m.end():end])
        check(rel, lineof(m.start()), owner, 'init', called)
    # var.member and var.member(...), with one hop through a known property type
    for m in re.finditer(r'(?<![.\w])([a-z][A-Za-z0-9_]*)\.([a-z_][A-Za-z0-9_]*)(?:\??\.([a-z_][A-Za-z0-9_]*))?', src):
        v, member, second = m.group(1), m.group(2), m.group(3)
        if v not in vars_:
            continue
        owner = vars_[v]
        if resolve(owner) is None:
            continue
        pos = m.start(2) + len(member)
        called = None
        if pos < len(src) and src[pos] == '(':
            called = call_labels(src[pos + 1:balanced(src, pos)])
        check(rel, lineof(m.start()), owner, member, called)
        if second and called is None:
            path = resolve(owner)
            ent = types.get(path, {}).get(member) if path else None
            if ent and ent[0]['rtype'] and not ent[0]['rtype'].startswith('['):
                pos2 = m.end()
                called2 = None
                if pos2 < len(src) and src[pos2] == '(':
                    called2 = call_labels(src[pos2 + 1:balanced(src, pos2)])
                check(rel, lineof(m.start()), ent[0]['rtype'], second, called2)
    # private members of other files' types used here
    for m in re.finditer(r'\b([A-Z][A-Za-z0-9_]*)\.([a-z_][A-Za-z0-9_]*)', src):
        path = resolve(m.group(1))
        if path and any(e['access'] in ('private', 'fileprivate') for e in types[path].get(m.group(2), [])) \
                and all(e['access'] in ('private', 'fileprivate') for e in types[path].get(m.group(2), [])):
            problems.append(f'{rel}:{lineof(m.start())}: {path}.{m.group(2)} is private to its file')

found = sorted(set(problems))
for p in found:
    print(p)
print(f'ui-symbols: checked {len(files)} SwiftUI files, {len(found)} possible problems')
sys.exit(1 if found else 0)
