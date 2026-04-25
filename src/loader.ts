const DIM = '\x1b[2m';
const RESET = '\x1b[0m';

const SPINNER_FRAMES = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const GRADIENT_COLORS = [
  '\x1b[38;5;240m', '\x1b[38;5;245m', '\x1b[38;5;250m',
  '\x1b[38;5;255m', '\x1b[38;5;250m', '\x1b[38;5;245m',
];

export type LoaderStyle = 'gradient' | 'spinner' | 'minimal';

export class Loader {
  private frame = 0;
  private interval: ReturnType<typeof setInterval> | null = null;

  constructor(private text: string, private style: LoaderStyle) {}

  start(): void {
    this.frame = 0;
    const ms = this.style === 'gradient' ? 150 : this.style === 'spinner' ? 80 : 300;
    this.draw();
    this.interval = setInterval(() => this.draw(), ms);
  }

  stop(): void {
    if (this.interval) {
      clearInterval(this.interval);
      this.interval = null;
      process.stdout.write('\r\x1b[K');
    }
  }

  setText(text: string): void { this.text = text; }

  private draw(): void {
    this.frame++;
    if (this.style === 'minimal') {
      const dots = ['·', '··', '···'];
      process.stdout.write(`\r${DIM}${this.text}${dots[this.frame % 3]}${RESET}`);
    } else if (this.style === 'spinner') {
      const char = SPINNER_FRAMES[this.frame % SPINNER_FRAMES.length];
      process.stdout.write(`\r${DIM}${char} ${this.text}${RESET}`);
    } else {
      const len = GRADIENT_COLORS.length;
      let out = '\r';
      for (let i = 0; i < this.text.length; i++) {
        out += GRADIENT_COLORS[(this.frame + i) % len] + this.text[i];
      }
      process.stdout.write(out + RESET);
    }
  }
}
