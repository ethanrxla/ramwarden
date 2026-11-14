from http.server import ThreadingHTTPServer, SimpleHTTPRequestHandler
import http.client
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1] / "web"
class Handler(SimpleHTTPRequestHandler):
    def __init__(self,*args,**kwargs):
        super().__init__(*args,directory=str(ROOT),**kwargs)
    def proxy(self):
        connection = http.client.HTTPConnection('127.0.0.1', 8080, timeout=30)
        try:
            length = int(self.headers.get('Content-Length', '0'))
            if length < 0 or length > 65536: return self.send_error(413)
            body = self.rfile.read(length)
            headers = {k:v for k,v in self.headers.items() if k.lower() not in ('host', 'connection', 'content-length')}
            connection.request(self.command, self.path, body=body, headers=headers)
            result = connection.getresponse()
            self.send_response(result.status)
            for key,value in result.getheaders():
                if key.lower() not in ('connection','transfer-encoding','content-length'): self.send_header(key,value)
            data=result.read()
            self.send_header('Content-Length',str(len(data)))
            self.end_headers(); self.wfile.write(data)
        except (OSError, http.client.HTTPException):
            self.send_error(503, 'Local account service is starting')
        finally: connection.close()
    def do_GET(self):
        if self.path.startswith('/v1/'): return self.proxy()
        return super().do_GET()
    def do_POST(self): return self.proxy() if self.path.startswith('/v1/') else self.send_error(404)
    def do_DELETE(self): return self.proxy() if self.path.startswith('/v1/') else self.send_error(404)
    def do_PUT(self): return self.proxy() if self.path.startswith('/v1/') else self.send_error(404)
    def translate_path(self,path):
        if path.startswith("/sales-preview/"): path=path[len("/sales-preview"):]
        return super().translate_path(path)
    def send_head(self):
        if 'If-Modified-Since' in self.headers: del self.headers['If-Modified-Since']
        return super().send_head()
    def end_headers(self):
        self.send_header('Cache-Control','no-store')
        super().end_headers()
ThreadingHTTPServer(('127.0.0.1',8000),Handler).serve_forever()
