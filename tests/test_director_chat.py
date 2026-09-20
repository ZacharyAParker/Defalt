import copy
import json
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from radio import config, db, director, director_chat, mixconfig, taste, timeline, vibe, wishes
from radio.app import app


class DirectorChatTests(unittest.TestCase):
    def setUp(self):
        tmp=tempfile.TemporaryDirectory();self.addCleanup(tmp.cleanup)
        local=threading.local()
        self.settings={}
        for p in [patch.object(db,'_DB_PATH',Path(tmp.name)/'test.db'),patch.object(db,'_LOCAL',local),
                  patch.object(config.station,'get',side_effect=lambda k,d=None:self.settings.get(k,d)),
                  patch.object(config.station,'set_many',side_effect=self.settings.update),
                  patch.object(config.station,'set',side_effect=lambda k,v:self.settings.update({k:v}))]:
            p.start();self.addCleanup(p.stop)
        self.addCleanup(lambda:getattr(local,'conn',None) and local.conn.close())
        vibe.set_session_selection(None);self.addCleanup(lambda:vibe.set_session_selection(None))
        self.station=SimpleNamespace(lock=threading.RLock(),clock=Mock(),schedule=timeline.Schedule(),_lineup=[],refresh_vibe=Mock())
        self.station.clock.now.return_value=10
        self.chat=director_chat.Chat(self.station)
        self.song={'key':'a','title':'Quiet Room','artist':'A Band','genre':'soul','bpm':85,'duration':180}
        db.write('INSERT INTO tracks(key,title,artist,genre,bpm,duration,added_at) VALUES(?,?,?,?,?,?,0)',('a','Quiet Room','A Band','soul',85,180))
        self.station.schedule.add_music('audio',self.song)

    def steer(self,description='More soul',**extra):
        return {'type':'steer','profile':{'description':description,'genres':['soul'],'avoid_genres':['rap'],'pace':'slow',**extra}}

    def test_private_direction_affects_selection_without_becoming_public_vibe(self):
        before=copy.deepcopy(self.station.schedule.items)
        self.chat.apply(self.steer('PRIVATE session detail'),self.chat.snapshot())
        self.assertEqual(vibe.for_selection()['description'],'PRIVATE session detail')
        self.assertFalse(vibe.public())
        self.assertFalse(self.settings)
        self.assertEqual(self.station.schedule.items,before)
        self.station.refresh_vibe.assert_called_once()
        self.assertGreater(vibe.fit({'genre':'soul'},vibe.for_selection()),vibe.fit({'genre':'rap'},vibe.for_selection()))

    def test_undo_restores_direction_without_erasing_saved_preference(self):
        self.settings['director_preferences.selection']={'description':'Saved jazz','genres':['jazz'],'pace':'any'}
        self.chat.apply(self.steer(),self.chat.snapshot())
        self.chat.apply({'type':'undo'},self.chat.snapshot())
        self.assertIsNone(vibe.session_selection())
        self.assertEqual(vibe.for_selection()['description'],'Saved jazz')

    def test_saved_direction_requires_explicit_save_flag_and_can_be_undone(self):
        self.chat.apply(self.steer(),self.chat.snapshot(),save=True)
        self.assertEqual(self.settings['director_preferences.selection']['description'],'More soul')
        self.chat.apply({'type':'undo'},self.chat.snapshot())
        self.assertEqual(self.settings['director_preferences.selection'],{})

    def test_set_vibe_takes_precedence_over_private_saved_and_session_directions(self):
        self.chat.apply(self.steer(),self.chat.snapshot(),save=True)
        self.chat.apply(self.steer('Private temporary direction'),self.chat.snapshot())
        with patch.object(config.station,'set_many',side_effect=self.settings.update):
            vibe.set_current('Jazz for dinner',enrich=False)
        self.assertEqual(vibe.for_selection()['description'],'Jazz for dinner')
        self.assertFalse(vibe.selection_direction())

    def test_reference_must_be_in_current_or_recent_context(self):
        self.chat.apply(self.steer(reference_key='a'),self.chat.snapshot())
        self.assertEqual(vibe.for_selection()['reference']['key'],'a')
        with self.assertRaises(ValueError):self.chat.apply(self.steer(reference_key='invented'),self.chat.snapshot())

    def test_quiet_expires_and_undo_restores_previous_deadline(self):
        self.chat.apply({'type':'quiet','minutes':20},self.chat.snapshot())
        self.assertGreater(self.chat.quiet_until,time.time()+1190)
        self.chat.apply({'type':'normal_talk'},self.chat.snapshot())
        self.assertEqual(self.chat.quiet_until,0)
        self.chat.apply({'type':'undo'},self.chat.snapshot())
        self.assertGreater(self.chat.quiet_until,time.time())
        with patch.object(director_chat.time,'time',return_value=self.chat.quiet_until+1):
            self.assertEqual(self.chat.state()['quiet_minutes'],0)

    def test_invalid_action_does_not_mutate_direction_or_schedule(self):
        for action in [{'type':'shell','command':'anything'},{'type':'quiet','minutes':999},{'type':'steer','profile':{'description':'unknown'}}]:
            with self.assertRaises(ValueError):self.chat.apply(action,self.chat.snapshot())
        self.assertIsNone(vibe.session_selection())
        self.assertFalse(self.chat.undo_stack)

    def test_request_is_queued_once_and_does_not_train_long_term_taste(self):
        action={'type':'request','title':'Quiet Room','artist':'A Band'}
        for _ in range(2):self.chat.apply(action,self.chat.snapshot())
        self.assertEqual(len(db.query('SELECT * FROM requests')),1)
        self.assertFalse(db.query('SELECT * FROM affinity'))

    def test_message_ids_are_idempotent_and_busy_rejects_extra_work(self):
        data={'id':'test-message-1','message':'More soul'}
        with patch.object(director_chat.threading.Thread,'start') as start:
            self.chat.submit(data);self.chat.submit(data)
            start.assert_called_once()
            with self.assertRaises(ValueError):self.chat.submit({'id':'test-message-2','message':'More jazz'})
        self.assertEqual(len(self.chat.messages),1)

    def test_long_pasted_script_is_accepted_whole_and_over_limit_is_rejected(self):
        script=('MAV\nA supplied draft line.\nRUE\nA response to that line.\n\n'*80)
        with patch.object(director_chat.threading.Thread,'start'):
            self.chat.submit({'id':'long-script-test','message':script})
        self.assertEqual(self.chat.messages[-1]['text'],director_chat.intent.clean(script))
        self.assertGreater(len(self.chat.messages[-1]['text']),2000)
        other=director_chat.Chat(self.station)
        with self.assertRaisesRegex(ValueError,'12,000'):
            other.submit({'id':'over-limit-test','message':'x'*12001})
        self.assertFalse(other.messages)

    def test_follow_up_uses_history_and_separate_writer_purpose(self):
        self.chat.messages=[{'role':'user','text':'Keep it soulful'},{'role':'director','text':'For this session: soul'}]
        with patch.object(director_chat.llm,'complete_json',return_value={'reply':'','action':self.steer()}) as model:
            self.chat._reply('test-0001','But slower',False,False)
        self.assertEqual(model.call_args.kwargs['purpose'],'director_chat')
        self.assertIn('Keep it soulful',model.call_args.args[1])
        self.assertFalse(self.chat.busy)
        self.assertFalse(db.query('SELECT * FROM wishes'))

    def test_sharing_is_explicit_and_delivered_as_listener_message(self):
        with patch.object(director_chat.llm,'complete_json') as model:
            self.chat._reply('test-0001','Love this set, Mav and Rue',False,True)
        model.assert_not_called()
        wish=wishes.next_segment()
        self.assertEqual(json.loads(wish['payload'])['segment'],'listener_message')

    def test_model_failure_changes_nothing_and_releases_busy_state(self):
        self.chat.busy=True
        with patch.object(director_chat.llm,'complete_json',return_value=None):
            self.chat._reply('test-0001','More soul',False,False)
        self.assertFalse(self.chat.busy)
        self.assertIsNone(vibe.session_selection())
        self.assertIn('Nothing changed',self.chat.messages[-1]['text'])

    def test_explicit_ad_commission_reaches_writer_without_sharing_private_history(self):
        message='Give them an ad prompt about something sarcastic related to recent gaming news'
        self.chat.messages=[{'role':'user','text':'PRIVATE unrelated detail'}]
        action={'type':'ad','brief':'Sarcastic fake ad about recent gaming news','news_category':'gaming','timing':'next_break'}
        manager=Mock()
        manager.queue.return_value={'state':'preparing','timing':'next_break','brief':action['brief']}
        with patch.object(director_chat.ads,'for_station',return_value=manager), \
             patch.object(director_chat.llm,'complete_json',return_value={'reply':'','action':action}) as model:
            self.chat._reply('ad-test-1',message,False,False)
        manager.queue.assert_called_once_with('next_break',brief=action['brief'],news_category='gaming')
        self.assertIn(message,model.call_args.args[1])
        self.assertNotIn('PRIVATE',str(manager.queue.call_args))
        self.assertIn('Ad brief accepted',self.chat.messages[-1]['text'])
        self.assertFalse(db.query('SELECT * FROM wishes'))
        self.assertFalse(self.chat.undo_stack)

    def test_ad_validation_and_busy_failure_are_reported_without_claiming_success(self):
        with self.assertRaises(ValueError):self.chat.apply({'type':'ad','brief':[]},self.chat.snapshot())
        with patch.object(director_chat.ads,'for_station') as factory, \
             patch.object(director_chat.llm,'complete_json',return_value={'action':{'type':'ad','brief':'A new premise'}}):
            factory.return_value.queue.side_effect=ValueError('An ad is already preparing. New brief was not added.')
            self.chat._reply('ad-test-2','Commission an ad',False,False)
        self.assertIn('not added',self.chat.messages[-1]['text'])
        self.assertNotIn('accepted',self.chat.messages[-1]['text'])

    def test_private_ad_brainstorm_does_not_queue_or_broadcast(self):
        with patch.object(director_chat.ads,'for_station') as factory, \
             patch.object(director_chat.llm,'complete_json',return_value={'action':{'type':'none'},'reply':'Here is a private draft.'}):
            self.chat._reply('ad-test-3','Just brainstorm an ad with me privately',False,False)
        factory.assert_not_called()
        self.assertFalse(db.query('SELECT * FROM wishes'))

    def test_back_to_normal_bypasses_both_directions_and_preserves_prepared_music_and_requests(self):
        self.settings['listening_vibe']={'id':'old-vibe','description':'Study music','genres':['ambient']}
        self.settings['director_preferences.selection']={'description':'Saved jazz','genres':['jazz']}
        self.chat.apply(self.steer(),self.chat.snapshot())
        saved=copy.deepcopy(self.settings)
        prepared=copy.deepcopy(self.station.schedule.items)
        self.station._lineup=[{'source':'auto'},{'source':'request'},{'source':'deck'}]
        self.station.refresh_vibe=lambda:director.Station.refresh_vibe(self.station)
        self.chat.quiet_until=time.time()+900
        deadline=self.chat.quiet_until
        with patch.object(director_chat.llm,'complete_json') as model:
            self.chat._reply('normal-test','Please go back to normal!',False,False)
        model.assert_not_called()
        self.assertEqual(vibe.for_selection(),{})
        self.assertEqual(vibe.public(),{})
        self.assertEqual(self.settings,saved)
        self.assertEqual(self.station.schedule.items,prepared)
        self.assertEqual([i['source'] for i in self.station._lineup],['request','deck'])
        self.assertEqual(self.chat.quiet_until,deadline)
        self.assertFalse(db.query('SELECT * FROM affinity'))
        self.chat.apply({'type':'undo'},self.chat.snapshot())
        self.assertEqual(vibe.for_selection()['description'],'More soul')
        self.assertEqual(vibe.public()['description'],'Study music')

    def test_saved_normal_can_be_undone_and_a_new_vibe_overrides_temporary_normal(self):
        self.settings.update({'listening_vibe':{'id':'x','description':'Night drive'},
                              'director_preferences.selection':{'description':'Saved soul','genres':['soul']}})
        before=copy.deepcopy(self.settings)
        self.chat.apply({'type':'normal'},self.chat.snapshot(),save=True)
        self.assertEqual(vibe.for_selection(),{})
        self.assertFalse(self.settings['listening_vibe'])
        self.assertFalse(self.settings['director_preferences.selection'])
        self.chat.apply({'type':'undo'},self.chat.snapshot())
        self.assertEqual(self.settings,before)
        self.chat.apply({'type':'normal'},self.chat.snapshot())
        vibe.set_current('Upbeat funk',enrich=False)
        self.assertEqual(vibe.for_selection()['description'],'Upbeat funk')
        self.assertIsNone(vibe.session_selection())

    def test_normal_talk_shortcut_does_not_reset_music(self):
        self.chat.apply(self.steer(),self.chat.snapshot())
        with patch.object(director_chat.llm,'complete_json') as model:
            self.chat._reply('normal-talk-test','Normal talk',False,False)
        model.assert_not_called()
        self.assertEqual(vibe.for_selection()['description'],'More soul')

    def test_endpoints_validate_before_starting_work(self):
        self.station.director_chat=self.chat
        with app.test_client() as client, patch('radio.app.director.station',return_value=self.station), patch.object(director_chat.threading.Thread,'start'):
            for payload in [None,[],{}, {'message':'hello','id':'bad'}, {'message':'hello','id':'valid-id-1','share':'yes'}]:
                self.assertEqual(client.post('/api/director/chat',json=payload).status_code,400)
            self.assertEqual(client.post('/api/director/chat',json={'message':'hello','id':'valid-id-1'}).status_code,202)
            self.assertTrue(client.get('/api/director/chat').json['busy'])
