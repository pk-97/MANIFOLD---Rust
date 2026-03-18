#!/bin/bash
cd "$(dirname "$0")"
PORT=8053
echo "Serving parity dashboard at http://localhost:$PORT/parity_dashboard.html"
open "http://localhost:$PORT/parity_dashboard.html"
python3 -m http.server $PORT --bind 127.0.0.1
