"""Library CRUD. Cue slots are 0–7; positions are native track samples.

PUT /api/crates/<id> accepts name and/or tracks (an ordered list of keys).
Replacing tracks handles addition, removal and reorder in one transaction.
"""
import sqlite3

import numpy as np
from flask import Blueprint, jsonify, request

from . import analysis, db

blueprint = Blueprint('library_api', __name__)


def payload():
    value = request.get_json(silent=True)
    if not isinstance(value, dict):
        raise ValueError('expected a JSON object')
    return value


@blueprint.errorhandler(ValueError)
def invalid(error):
    return jsonify(error=str(error)), 400


@blueprint.errorhandler(sqlite3.IntegrityError)
def integrity(error):
    return jsonify(error='unknown track or invalid membership'), 400


@blueprint.get('/api/library')
def library_listing():
    """Everything a deck can be loaded with: local files, on this disk, now.

    Fetched records are excluded deliberately -- they are the radio's cache
    and may be deleted out from under a deck at any time.
    """
    rows = db.query(
        "SELECT key, artist, title, album, genre, year, duration, bpm, camelot, "
        "lufs, file, beat_offset, beat_period, downbeat_offset "
        "FROM tracks WHERE source='local' AND file IS NOT NULL "
        "ORDER BY artist, title")
    return jsonify(tracks=[dict(row) for row in rows])


@blueprint.get('/api/tracks/<path:key>/peaks')
def track_peaks(key):
    """Waveform for drawing, reduced to however many buckets were asked for.

    Reducing here rather than in the browser: the finest level of a five
    minute record is around 26,000 buckets, and a canvas a thousand pixels
    wide has no use for the other 25,000.
    """
    row = db.one('SELECT file FROM tracks WHERE key=?', (key,))
    if row is None or not row['file']:
        return jsonify(error='unknown track'), 404

    try:
        buckets = min(max(int(request.args.get('buckets', 1200)), 16), 8000)
    except (TypeError, ValueError):
        raise ValueError('buckets must be a number')

    try:
        data = analysis.peaks(row['file'])
    except (OSError, ValueError) as error:
        return jsonify(error=str(error)), 404

    # Coarsest level that still has more detail than we were asked for.
    levels = data['levels']
    usable = [width for width in sorted(levels) if len(levels[width]) >= buckets]
    source = levels[usable[-1]] if usable else levels[min(levels)]

    edges = np.linspace(0, len(source), buckets + 1).astype(int)
    out = []
    for start, end in zip(edges[:-1], edges[1:]):
        block = source[start:max(end, start + 1)]
        out.append([
            float(block[:, 0].min()), float(block[:, 1].max()),
            float(block[:, 2].mean()), float(block[:, 3].mean()),
            float(block[:, 4].mean()),
        ])

    return jsonify(
        seconds=data['sample_count'] / data['sample_rate'],
        buckets=out,
    )


@blueprint.route('/api/tracks/<path:key>/cues', methods=['GET'])
@blueprint.route('/api/tracks/<path:key>/cues/<int:slot>', methods=['PUT', 'DELETE'])
def cues(key, slot=None):
    if not db.one('SELECT 1 FROM tracks WHERE key=?', (key,)):
        return jsonify(error='unknown track'), 404
    if slot is not None and not 0 <= slot < 8:
        raise ValueError('slot must be from 0 to 7')
    if request.method == 'PUT':
        data = payload()
        db.set_hot_cue(key, slot, data.get('position_samples'),
                       data.get('colour', '#ffffff'), data.get('label', ''))
    elif request.method == 'DELETE':
        db.delete_hot_cue(key, slot)
    return jsonify(cues=db.hot_cues(key))


@blueprint.route('/api/crates', methods=['GET', 'POST'])
def crates():
    if request.method == 'GET':
        return jsonify(crates=[dict(row) for row in db.query('SELECT * FROM crates ORDER BY id')])
    data = payload()
    ident = db.save_crate(data.get('name'), data.get('tracks', []))
    return jsonify(db.crate(ident)), 201


@blueprint.route('/api/crates/<int:ident>', methods=['GET', 'PUT', 'DELETE'])
def crate(ident):
    existing = db.crate(ident)
    if existing is None:
        return jsonify(error='unknown crate'), 404
    if request.method == 'DELETE':
        db.delete_crate(ident)
        return jsonify(ok=True)
    if request.method == 'PUT':
        data = payload()
        db.save_crate(data.get('name', existing['name']),
                      data.get('tracks', [t['key'] for t in existing['tracks']]), ident)
    return jsonify(db.crate(ident))
