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
    def track(before, after, point):
        return np.array(point)+c.calibration@c.current
    monkeypatch.setattr('camera_mcp.track_point',track)
    metadata, image = c.focus(.7,.3,'frame')
    assert metadata['focus_status'] == 'centered'
    assert np.allclose(metadata['target'],[.5,.5])
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
    assert c.goto.call_count == 1


@pytest.mark.parametrize('point',[(float('nan'),.5),(-.1,.5),(.5,1.1)])
def test_focus_rejects_invalid_points(point):
    c = simulated_camera()
    with pytest.raises(ValueError): c.focus(*point,'frame')
    c.goto.assert_not_called()
