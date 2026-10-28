import base64
from io import BytesIO
import json
from PIL import Image
from camera_mcp import observation_result


def observation():
    buffer = BytesIO()
    Image.new('RGB',(1280,720),'orange').save(buffer,format='JPEG')
    metadata={'frame_id':'same-frame','width':1280,'height':720}
    return metadata,buffer.getvalue()


def test_preview_downsamples_and_keeps_frame_identity():
    metadata,jpeg=observation()
    result=observation_result((metadata,jpeg))
    returned=json.loads(result.content[0].text)
    image=Image.open(BytesIO(base64.b64decode(result.content[1].data)))
    assert image.size == (640,360)
    assert (returned['width'],returned['height']) == image.size
    assert returned['frame_id'] == metadata['frame_id']
    assert returned['source_width'] == 1280
    assert metadata['width'] == 1280  # Internal tracking pixels stay unchanged.


def test_full_detail_remains_available():
    result=observation_result(observation(),detail=True)
    returned=json.loads(result.content[0].text)
    image=Image.open(BytesIO(base64.b64decode(result.content[1].data)))
    assert image.size == (1280,720)
    assert returned['detail'] == 'full'
