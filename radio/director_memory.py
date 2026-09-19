"""Read reviewed listener facts without importing source documents or private paths."""
from __future__ import annotations
import json
from pathlib import Path
import re
import yaml

class MemoryUnavailable(ValueError):
    pass

def load_memory(root: Path) -> list[dict]:
    """Fail closed on malformed approved notes; never ingest sources or links."""
    root = root.resolve()
    notes = []
    for path in sorted((root / 'Notes').glob('*.md')):
        resolved = path.resolve()
        if not resolved.is_relative_to(root) or path.is_symlink():
            raise MemoryUnavailable('Memory link escapes its vault')
        if path.stat().st_size > 16_384:
            raise MemoryUnavailable('Memory note exceeds size limit')
        raw = path.read_text(encoding='utf-8-sig')
        chunks = raw.split('---', 2)
        if len(chunks) != 3 or chunks[0].strip():
            raise MemoryUnavailable('Memory note needs YAML frontmatter')
        meta = yaml.safe_load(chunks[1])
        if not isinstance(meta, dict):
            raise MemoryUnavailable('Invalid note properties')
        if meta.get('broadcast') is not True or meta.get('status') != 'active':
            continue
        if meta.get('basis') not in {'direct', 'documented'} or not meta.get('sources'):
            raise MemoryUnavailable('Approved notes need a reviewed basis and sources')
        note_id = meta.get('id', '')
        keywords = meta.get('keywords', [])
        if not isinstance(note_id, str) or not re.fullmatch(r'[a-z][a-z0-9-]{0,59}', note_id):
            raise MemoryUnavailable('Invalid memory id')
        if not isinstance(keywords, list) or any(not isinstance(k, str) for k in keywords):
            raise MemoryUnavailable('Memory keywords must be a list of strings')
        match = re.search(r'^## Facts\s*\n(.*?)(?=^## |\Z)', chunks[2], re.M | re.S)
        if not match or not match[1].strip():
            raise MemoryUnavailable('Approved note has no Facts section')
        notes.append({'id': note_id, 'facts': match[1].strip(),
                      'core': meta.get('core') is True, 'keywords': keywords})
    if len(notes) > 64 or len({n['id'] for n in notes}) != len(notes):
        raise MemoryUnavailable('Memory collection is empty, too large, or has duplicate ids')
    return notes


def retrieve(notes: list[dict], context: dict) -> list[dict]:
    query = json.dumps(context, ensure_ascii=False).lower()
    def score(note):
        return sum(bool(re.search(r'(?<!\w)' + re.escape(k.lower()) + r'(?!\w)', query))
                   for k in note['keywords'] if k)
    selected = [n for n in notes if n['core']]
    extras = sorted((n for n in notes if not n['core'] and score(n)),
                    key=lambda n: (-score(n), n['id']))[:2]
    selected += extras
    result = [{'id': n['id'], 'facts': n['facts']} for n in selected]
    if len(json.dumps(result)) > 12_000:
        raise MemoryUnavailable('Retrieved memory exceeds prompt budget')
    return result
