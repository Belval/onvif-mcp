"""Bounded stream-resolution setup and restoration for the historical rerun.

Run with the repository's historical .venv (onvif-zeep). Never changes credentials,
PTZ or main-stream settings. Backup is written before any mutation.
"""
import argparse
import copy
from datetime import timedelta
import json
from pathlib import Path
import isodate
import requests
from dotenv import dotenv_values
from onvif import ONVIFCamera
from zeep.helpers import serialize_object
from zeep.transports import Transport

ROOT=Path(__file__).resolve().parents[2]
WORK=ROOT/'captures/commit-timings-2026-11-26'

def connect(host):
    cfg=dotenv_values(ROOT/'.env');session=requests.Session();session.trust_env=False
    camera=ONVIFCamera(host,int(cfg.get('CAMERA_PORT','2020')),cfg['CAMERA_USERNAME'],cfg['CAMERA_PASSWORD'],no_cache=True,adjust_time=True,transport=Transport(session=session,timeout=6,operation_timeout=6))
    return camera,camera.create_media_service()

def default(value):
    if isinstance(value,timedelta):return isodate.duration_isoformat(value)
    raise TypeError(type(value).__name__)

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action',choices=['prepare','restore'])
    parser.add_argument('--host',required=True)
    args=parser.parse_args();WORK.mkdir(parents=True,exist_ok=True)
    backup=WORK/'stream-backup.json'
    camera,media=connect(args.host)
    if args.action=='prepare':
        profiles=media.GetProfiles()
        profile=next(p for p in profiles if p.Name=='minorStream')
        cfg=media.GetVideoEncoderConfiguration({'ConfigurationToken':profile.VideoEncoderConfiguration.token})
        if backup.exists():raise RuntimeError('Backup already exists; restore or audit it before another setup')
        info=camera.devicemgmt.GetDeviceInformation()
        saved={'host':args.host,'model':info.Model,'firmware':info.FirmwareVersion,'hardware':info.HardwareId,'configuration':serialize_object(cfg),'restored':False}
        backup.write_text(json.dumps(saved,indent=2,default=default)+'\n');backup.chmod(0o600)
        options=media.GetVideoEncoderConfigurationOptions({'ConfigurationToken':cfg.token,'ProfileToken':profile.token})
        supported={(r.Width,r.Height) for r in options.H264.ResolutionsAvailable}
        if (640,360) not in supported:raise RuntimeError('640x360 is not advertised for this encoder')
        changed=copy.deepcopy(cfg);changed.Resolution.Width=640;changed.Resolution.Height=360
        media.SetVideoEncoderConfiguration({'Configuration':changed,'ForcePersistence':False})
        got=media.GetVideoEncoderConfiguration({'ConfigurationToken':cfg.token})
        assert (got.Resolution.Width,got.Resolution.Height)==(640,360),'Resolution change not applied'
        saved['applied_configuration']=serialize_object(got)
        backup.write_text(json.dumps(saved,indent=2,default=default)+'\n')
        print('Minor stream set to 640x360; original complete configuration saved.')
    else:
        saved=json.loads(backup.read_text())
        if saved['host']!=args.host:raise RuntimeError('Backup belongs to another host')
        media.SetVideoEncoderConfiguration({'Configuration':saved['configuration'],'ForcePersistence':False})
        got=serialize_object(media.GetVideoEncoderConfiguration({'ConfigurationToken':saved['configuration']['token']}))
        normalized=json.loads(json.dumps(got,default=default))
        assert normalized==saved['configuration'],'Restored configuration differs from backup'
        saved['restored']=True;backup.write_text(json.dumps(saved,indent=2)+'\n')
        print('Complete original minor-stream configuration restored and verified.')

if __name__=='__main__':main()
