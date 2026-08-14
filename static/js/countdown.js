// 倒计时函数：expireAt 为 Unix 时间戳（秒）
function startCountdown(expireAt, displayElement) {
    if (!displayElement) return;
    if (typeof expireAt !== 'number' || isNaN(expireAt) || expireAt <= 0) {
        displayElement.textContent = '无效时间';
        return;
    }

    function updateTimer() {
        const now = Math.floor(Date.now() / 1000);
        let remaining = expireAt - now;
        if (remaining <= 0) {
            displayElement.textContent = '已过期，请重新下单';
            clearInterval(interval);
            return;
        }
        const mins = Math.floor(remaining / 60);
        const secs = remaining % 60;
        displayElement.textContent = 
            `${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}`;
    }

    updateTimer();
    const interval = setInterval(updateTimer, 1000);
}
