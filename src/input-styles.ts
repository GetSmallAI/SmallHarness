const GRAY = '\x1b[90m';
const RESET = '\x1b[0m';

export function borderedReadLine(borderColor = GRAY): Promise<string> {
  return new Promise((resolve) => {
    let line = '';
    let first = true;
    const width = process.stdout.columns || 80;
    const border = `${borderColor}${'─'.repeat(width)}${RESET}`;

    function draw() {
      if (first) {
        process.stdout.write(`\n${border}\n`);
        process.stdout.write(`› ${line}\n`);
        process.stdout.write(`${border}\x1b[1A\r\x1b[${3 + line.length}G`);
        first = false;
      } else {
        process.stdout.write(`\r\x1b[2K`);
        process.stdout.write(`› ${line}`);
      }
    }

    draw();
    process.stdin.setRawMode(true);
    process.stdin.resume();

    const onData = (data: Buffer) => {
      const str = data.toString('utf-8');
      if (str.startsWith('\x1b')) return;
      for (let i = 0; i < str.length; i++) {
        const code = str.charCodeAt(i);
        if (code === 13 || code === 10) {
          process.stdin.off('data', onData);
          process.stdin.setRawMode(false);
          process.stdin.pause();
          if (!line) {
            process.stdout.write(`\x1b[1A\x1b[2K\x1b[1A\x1b[2K\r`);
          } else {
            process.stdout.write(`\x1b[1B\x1b[2K\r`);
          }
          resolve(line);
          return;
        } else if (code === 127 || code === 8) {
          line = line.slice(0, -1);
          draw();
        } else if (code === 3) {
          process.stdout.write(`${RESET}\n`);
          process.exit(0);
        } else if (code >= 32) {
          line += str[i];
          draw();
        }
      }
    };

    process.stdin.on('data', onData);
  });
}

export function plainReadLine(prompt = '> '): Promise<string> {
  return new Promise((resolve) => {
    process.stdout.write(prompt);
    let line = '';
    process.stdin.setRawMode(true);
    process.stdin.resume();
    const onData = (data: Buffer) => {
      const str = data.toString('utf-8');
      for (let i = 0; i < str.length; i++) {
        const code = str.charCodeAt(i);
        if (code === 13 || code === 10) {
          process.stdin.off('data', onData);
          process.stdin.setRawMode(false);
          process.stdin.pause();
          process.stdout.write('\n');
          resolve(line);
          return;
        } else if (code === 127 || code === 8) {
          if (line.length) { line = line.slice(0, -1); process.stdout.write('\b \b'); }
        } else if (code === 3) {
          process.stdout.write('\n');
          process.exit(0);
        } else if (code >= 32) {
          line += str[i];
          process.stdout.write(str[i]);
        }
      }
    };
    process.stdin.on('data', onData);
  });
}
