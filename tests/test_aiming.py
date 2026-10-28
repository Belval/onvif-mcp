import time
from unittest.mock import Mock
import cv2
import numpy as np
import pytest
from aiming import track_point
from camera_mcp import Camera


def jpeg(image):
    return cv2.imencode('.jpg', image)[1].tobytes()


def textured_image():
    rng = np.random.default_rng(42)
    image = np.full((720,1280), 128, np.uint8)
    for _ in range(1200):
        x,y = rng.integers([15,15],[1265,705])
        cv2.circle(image, (int(x),int(y)), int(rng.integers(2,7)), int(rng.integers(0,256)), -1)
    return image


def test_point_tracking_follows_camera_image_translation():
    before = textured_image()
    after = cv2.warpAffine(before, np.float32([[1,0,65],[0,1,-40]]), (1280,720))
    point = track_point(jpeg(before), jpeg(after), [.5,.5])
    assert point is not None
    assert np.allclose(point, [.5+65/1280,.5-40/720], atol=.006)


def test_textureless_target_does_not_claim_tracking():
    blank = jpeg(np.zeros((720,1280),np.uint8))
    assert track_point(blank,blank,[.5,.5]) is None


def simulated_camera(point=(.7,.3)):
    c = Camera.__new__(Camera)
    c.calibration = np.array([[1.7,0],[0,.9]])
    c.observation = {'id':'frame','time':time.monotonic(), 'pose':np.array([0.,0.]), 'jpeg':jpeg(textured_image())}
    c.current = np.array([0.,0.])
    c.position = lambda: c.current.copy()
    c.stop = Mock()
    def goto(target):
        c.current = np.array(target)
        return c.current.copy()
    c.goto = Mock(side_effect=goto)
    c.see = Mock(return_value=c.observation['jpeg'])
    return c


def test_focus_uses_position_mapping_and_returns_centered_image(monkeypatch):
    c = simulated_camera()
    previous = np.zeros(2)
    def track(before, after, point):
        nonlocal previous
        result = np.array(point)+c.calibration@(c.current-previous)
        previous = c.current.copy()
        return result
    monkeypatch.setattr('camera_mcp.track_point',track)
    metadata, image = c.focus(.7,.3,'frame')
    assert metadata['focus_status'] == 'centered'
    assert np.linalg.norm(np.array(metadata['target'])-.5) <= .02
    target = c.goto.call_args.args[0]
    assert target[0] < 0  # Image-right requires decreasing this camera's pan position.
    assert target[1] > 0  # Image-above requires increasing tilt position.
    assert image and metadata['frame_id'] != 'frame'


def test_stale_image_never_moves_camera():
    c = simulated_camera()
    c.observation['time'] -= 121
    metadata, _ = c.focus(.7,.3,'frame')
    assert metadata['focus_status'] == 'stale_frame'
    c.goto.assert_not_called()


def test_external_movement_invalidates_image():
    c = simulated_camera()
    c.current[0] = .1
    metadata, _ = c.focus(.7,.3,'frame')
    assert metadata['focus_status'] == 'stale_frame'
    c.goto.assert_not_called()


def test_tracking_failure_stops_corrections(monkeypatch):
    c = simulated_camera()
    monkeypatch.setattr('camera_mcp.track_point',lambda *args:None)
    metadata, _ = c.focus(.7,.3,'frame')
    assert metadata['focus_status'] == 'tracking_lost'
    assert metadata['target'] is None
    assert c.goto.call_count == 0


@pytest.mark.parametrize('point',[(float('nan'),.5),(-.1,.5),(.5,1.1)])
def test_focus_rejects_invalid_points(point):
    c = simulated_camera()
    with pytest.raises(ValueError): c.focus(*point,'frame')
    c.goto.assert_not_called()


def test_absolute_move_waits_for_stable_position_and_stops(monkeypatch):
    c = Camera.__new__(Camera)
    c.profile = Mock(token='profile')
    c.ptz = Mock()
    c.stop = Mock()
    c.position = Mock(side_effect=[np.array([0.,0.]),np.array([.1,.2]),np.array([.1,.2])])
    monkeypatch.setattr('camera_mcp.time.sleep',lambda _:None)
    assert np.allclose(c.goto([.1,.2]),[.1,.2])
    assert c.position.call_count == 3
    c.stop.assert_called_once()
    request = c.ptz.AbsoluteMove.call_args.args[0]
    assert 'Speed' not in request


def test_absolute_move_failure_still_stops():
    c = Camera.__new__(Camera)
    c.profile = Mock(token='profile')
    c.ptz = Mock()
    c.ptz.AbsoluteMove.side_effect = RuntimeError('disconnected')
    c.stop = Mock()
    with pytest.raises(RuntimeError): c.goto([.1,.2])
    c.stop.assert_called_once()


def test_visual_dead_zone_does_not_amplify_corrections(monkeypatch):
    c = simulated_camera()
    monkeypatch.setattr('camera_mcp.track_point',lambda a,b,point:np.array(point))
    metadata, _ = c.focus(.6,.6,'frame')
    assert metadata['focus_status'] == 'iteration_limit'
    positions = np.array([call.args[0] for call in c.goto.call_args_list])
    increments = np.diff(np.vstack([np.zeros(2),positions]),axis=0)
    assert np.allclose(increments, increments[0])
