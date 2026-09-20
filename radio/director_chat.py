"""Private conversational controls. Only validated station actions can be applied."""
from __future__ import annotations
import copy
import json
import re
import threading
import time
from pathlib import Path

from . import config, db, intent, llm, taste, vibe, wishes

SYSTEM = (Path(__file__).parent / 'prompts/director-chat.md').read_text(encoding='utf-8')


def profile(value):
    if not isinstance(value, dict):
        raise ValueError('The music direction was incomplete. Try describing the sound again.')
    result = {'description': intent.clean(value.get('description', ''))[:240], 'private': True}
    if not result['description']:
        raise ValueError('Describe the music direction you want.')
    for key in ('genres', 'avoid_genres'):
        values = value.get(key, [])
        if not isinstance(values, list) or len(values) > 8 or any(not isinstance(v,str) for v in values):
            raise ValueError('The genre direction was invalid. Please try again.')
        result[key] = [intent.clean(v)[:50] for v in values if intent.clean(v)]
    if value.get('pace', 'any') not in {'slow','medium','fast','any'}:
        raise ValueError('The pace was invalid. Please try again.')
    result['pace'] = value.get('pace','any')
    return result


class Chat:
    def __init__(self, station):
        self.station = station
        self.lock = threading.RLock()
        self.messages = []
        self.jobs = {}
        self.busy = False
        self.quiet_until = 0.0
        self.undo_stack = []

    def state(self):
        with self.lock:
            return {'messages': copy.deepcopy(self.messages), 'busy': self.busy,
                    'direction': vibe.selection_direction(),
                    'quiet_minutes': max(0, round((self.quiet_until-time.time())/60,1)),
                    'can_undo': bool(self.undo_stack)}

    def submit(self, data):
        if not isinstance(data,dict):
            raise ValueError('Send a message object.')
        message = data.get('message')
        ident = data.get('id')
        if not isinstance(message,str) or not 1 <= len(message.strip()) <= 2000:
            raise ValueError('Write a message of 1 to 2,000 characters.')
        if not isinstance(ident,str) or not re.fullmatch(r'[A-Za-z0-9_-]{8,80}',ident):
            raise ValueError('A message ID is required.')
        for key in ('save','share'):
            if type(data.get(key,False)) is not bool:
                raise ValueError('Choose true or false for message options.')
        with self.lock:
            if ident in self.jobs:
                return self.state()
            if self.busy:
                raise ValueError('The director is replying. Send your follow-up when it finishes.')
            self.busy = True
            self.jobs[ident] = True
            if len(self.jobs) > 200:
                self.jobs.pop(next(iter(self.jobs)))
            self.messages.append({'id':ident,'role':'user','text':intent.clean(message),
                                  'shared':data.get('share',False)})
            self.messages = self.messages[-40:]
        threading.Thread(target=self._reply, args=(ident,intent.clean(message),data.get('save',False),data.get('share',False)),
                         daemon=True,name='private-director-chat').start()
        return self.state()

    def snapshot(self):
        s = self.station
        with s.lock:
            now = s.clock.now()
            music = s.schedule.music_items()
            current = [i for i in music if i.start_at <= now < i.end_at]
            def track(item):
                meta = item.meta
                return {k:meta.get(k) for k in ('key','title','artist','genre','bpm','selection','selection_origin')}
            return {'playing': [track(i) for i in current],
                    'prepared_next': [track(i) for i in music if i.start_at > now][:3],
                    'recent': [track(i) for i in music if i.end_at <= now][-5:],
                    'unprepared_queue': [{k:e.get('track',{}).get(k) for k in ('key','title','artist')}
                                         for e in getattr(s,'_lineup',[])[:8]],
                    'direction': vibe.selection_direction(), 'public_vibe':vibe.public(),
                    'quiet_minutes':max(0,round((self.quiet_until-time.time())/60,1))}

    def _reply(self, ident, message, save, share):
        try:
            if share:
                error = intent.screen(message)
                if error:
                    raise ValueError(error)
                parsed = intent.Intent(kind='segment',subject=message,raw=message,segment='listener_message')
                wishes.add_wish(parsed, {'segment':'listener_message','message':message})
                reply = 'Sent to Mav and Rue for their next unwritten host break. Already prepared speech will finish first.'
            else:
                snapshot = self.snapshot()
                with self.lock:
                    history = copy.deepcopy(self.messages[-24:])
                lowered = message.lower().strip(' .!')
                action = None
                if lowered in {'undo','undo that','undo last change'}:
                    action = {'type':'undo'}
                elif lowered in {'clear direction','clear vibe','back to normal music'}:
                    action = {'type':'clear'}
                elif lowered in {'resume talking','normal talk','resume normal talk'}:
                    action = {'type':'normal_talk'}
                if action is None:
                    plan = llm.complete_json(SYSTEM,json.dumps({'conversation':history,'current':snapshot,
                        'scope':'saved' if save else 'session'},ensure_ascii=False),purpose='director_chat',
                        timeout=30,max_tokens=900,temperature=.35)
                    if not isinstance(plan,dict) or not isinstance(plan.get('action'),dict):
                        raise ValueError('The director could not interpret that just now. Nothing changed; try again.')
                    action = plan['action']
                else:
                    plan = {}
                reply = self.apply(action, snapshot, save, plan.get('reply',''))
        except Exception as error:
            reply = str(error) if isinstance(error,ValueError) else 'The director could not finish that change. Please try again.'
        finally:
            with self.lock:
                self.messages.append({'id':ident+'-reply','role':'director','text':reply[:2500]})
                self.messages = self.messages[-40:]
                self.busy = False

    def apply(self, action, snapshot, save=False, explanation=''):
        kind = action.get('type')
        if kind == 'none':
            if not isinstance(explanation,str) or not explanation.strip():
                raise ValueError('Could you describe the change you want? Nothing changed.')
            return explanation.strip()[:2000] + '\n\nNo settings or queue changes.'
        if kind == 'request':
            title, artist = action.get('title'),action.get('artist')
            if not all(isinstance(v,str) and 0 < len(v.strip()) <= 160 for v in (title,artist)):
                raise ValueError('Tell me the song title and artist so I can request the right recording.')
            title,artist = intent.clean(title),intent.clean(artist)
            waiting=db.one("SELECT COUNT(*) AS n FROM requests WHERE status IN ('pending','preparing')")
            if waiting and waiting['n'] >= 12:
                raise ValueError('Twelve requests are already waiting. Let some play or remove one first.')
            key=taste.add_track(title,artist,source='request')
            existing=db.one("SELECT id FROM requests WHERE track_key=? AND status IN ('pending','preparing','queued','scheduled')",(key,))
            if existing:
                return f'{title} by {artist} is already requested. I kept its place.'
            db.write("INSERT INTO requests(ts,query,status,track_key) VALUES(?,?,'pending',?)",
                     (time.time(),f'{artist} - {title}',key))
            return f'Requested {title} by {artist}. It will prepare after the songs already planned. Your long-term taste scores are unchanged.'
        if kind not in {'steer','quiet','normal_talk','clear','undo'}:
            raise ValueError('That control is not available here. Nothing changed.')
        if kind == 'undo':
            if not self.undo_stack:
                return 'There is no direction or talk change to undo. Song requests can be removed in the queue.'
            old=self.undo_stack[-1]
            if old['saved_changed']:
                config.station.set('director_preferences.selection',old['saved'])
            vibe.set_session_selection(old['session'])
            self.quiet_until=old['quiet']
            self.undo_stack.pop()
            self.station.refresh_vibe()
            return 'Undid the last direction or talk change. Prepared songs and speech keep their places.'
        new_profile = None
        if kind == 'steer':
            new_profile = profile(action.get('profile'))
            reference = action.get('profile',{}).get('reference_key')
            if reference:
                known = next((t for t in snapshot['playing']+snapshot['recent'] if t.get('key')==reference),None)
                if not known:
                    raise ValueError('That reference song is no longer in the current context. Name the song you meant.')
                row = db.one('SELECT key,title,artist,genre,bpm FROM tracks WHERE key=?',(reference,))
                if row:
                    new_profile['reference'] = dict(row)
            if not new_profile['genres'] and not new_profile['avoid_genres'] and new_profile['pace']=='any' and 'reference' not in new_profile:
                raise ValueError('Give me a genre, pace, or reference song so the direction can influence the picks.')
        minutes = action.get('minutes')
        if kind == 'quiet' and (type(minutes) not in (int,float) or not 1 <= minutes <= 120):
            raise ValueError('Choose between one and 120 minutes of fewer host breaks.')
        old={'session':vibe.session_selection(),'saved':copy.deepcopy(config.station.get('director_preferences.selection',{})),
             'quiet':self.quiet_until,'saved_changed':save and kind in {'steer','clear'}}
        if kind in {'steer','clear'}:
            direction=new_profile if kind=='steer' else {}
            if save:
                config.station.set('director_preferences.selection',direction)
                vibe.set_session_selection(None)
            else:
                vibe.set_session_selection(direction)
            self.station.refresh_vibe()
            detail = new_profile['description'] if new_profile else 'private direction cleared; your existing Set vibe still applies'
            reply = f"{'Saved' if save else 'For this session'}: {detail}. This starts with unprepared automatic picks; current songs, prepared transitions and your requests stay in place."
        elif kind=='quiet':
            self.quiet_until=time.time()+minutes*60
            reply=f'Fewer automatic host breaks for {minutes:g} minutes. Already prepared speech and explicitly requested segments still play.'
        else:
            self.quiet_until=0
            reply='Normal automatic host breaks will resume when the next ones are prepared.'
        self.undo_stack.append(old)
        self.undo_stack=self.undo_stack[-10:]
        return reply


def for_station(station):
    with station.lock:
        if not hasattr(station,'director_chat'):
            station.director_chat=Chat(station)
        return station.director_chat
