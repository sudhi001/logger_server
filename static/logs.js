async function fetchRecentLogs() {
    try {
        console.log('Fetching recent logs...');
        const response = await fetch('/logs/recent');
        if (!response.ok) {
            throw new Error(`HTTP error! status: ${response.status}`);
        }
        const logs = await response.json();
        const logsContainer = document.getElementById('logs');
        logs.reverse().forEach(log => {
            const logElement = document.createElement('div');
            logElement.className = 'log-entry p-2 border-b border-gray-700';
            logElement.innerText = `${log.name}${log.message}`;
            logsContainer.appendChild(logElement);
        });
        console.log('Recent logs fetched and displayed.');
    } catch (error) {
        console.error('Error fetching recent logs:', error);
    }
}

document.addEventListener('DOMContentLoaded', (event) => {
    fetchRecentLogs();
});

const eventSource = new EventSource('/logs/stream');

eventSource.onmessage = function(event) {
    const log = JSON.parse(event.data);
    const logElement = document.createElement('div');
    logElement.className = 'log-entry p-2 border-b border-gray-700';
    logElement.innerText = `${log.name}${log.message}`;
    const logsContainer = document.getElementById('logs');
    logsContainer.insertBefore(logElement, logsContainer.firstChild);
};

eventSource.onerror = function(err) {
    console.error('EventSource failed:', err);
};
