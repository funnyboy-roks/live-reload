(() => {
    let url = new URL(window.location);
    url.pathname = '/ws';
    url.protocol = 'ws:';

    let socket = new WebSocket(url);
    console.log('Connecting to WebSocket...');
    socket.addEventListener('open', () => {
        console.log('Connected.');
    });

    socket.addEventListener('message', (msg) => {
        console.log(msg);
        window.location.reload();
    });

    // window.scrollTo(0, document.body.scrollHeight);
    console.log(socket);

    window.socket = socket;
})()
